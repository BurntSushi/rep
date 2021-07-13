use std::cmp;

use bstr::ByteSlice;

use crate::line_buffer::BinaryDetection;
use crate::lines::{self, LineStep};
use crate::searcher::{Config, Range, Searcher};
use crate::sink::{
    Sink, SinkContext, SinkContextKind, SinkError, SinkFinish, SinkMatch,
};
use grep_matcher::{LineMatchKind, Matcher};

enum FastMatchResult {
    Continue,
    Stop,
    SwitchToSlow,
}

#[derive(Debug)]
pub struct Core<'s, M: 's, S> {
    config: &'s Config,
    matcher: M,
    searcher: &'s Searcher,
    sink: S,
    binary: bool,
    pos: usize,
    absolute_byte_offset: u64,
    binary_byte_offset: Option<usize>,
    line_number: Option<u64>,
    last_line_counted: usize,
    last_line_visited: usize,
    after_context_left: usize,
    has_sunk: bool,
    has_matched: bool,
}

impl<'s, M: Matcher, S: Sink> Core<'s, M, S> {
    pub fn new(
        searcher: &'s Searcher,
        matcher: M,
        sink: S,
        binary: bool,
    ) -> Core<'s, M, S> {
        let line_number =
            if searcher.config.line_number { Some(1) } else { None };
        let core = Core {
            config: &searcher.config,
            matcher: matcher,
            searcher: searcher,
            sink: sink,
            binary: binary,
            pos: 0,
            absolute_byte_offset: 0,
            binary_byte_offset: None,
            line_number: line_number,
            last_line_counted: 0,
            last_line_visited: 0,
            after_context_left: 0,
            has_sunk: false,
            has_matched: false,
        };
        if !core.searcher.multi_line_with_matcher(&core.matcher) {
            if core.is_line_by_line_fast() {
                log::trace!("searcher core: will use fast line searcher");
            } else {
                log::trace!("searcher core: will use slow line searcher");
            }
        }
        core
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn set_pos(&mut self, pos: usize) {
        self.pos = pos;
    }

    pub fn binary_byte_offset(&self) -> Option<u64> {
        self.binary_byte_offset.map(|offset| offset as u64)
    }

    pub fn matcher(&self) -> &M {
        &self.matcher
    }

    pub fn matched(
        &mut self,
        buf: &[u8],
        range: &Range,
    ) -> Result<bool, S::Error> {
        self.sink_matched(buf, range)
    }

    pub fn binary_data(
        &mut self,
        binary_byte_offset: u64,
    ) -> Result<bool, S::Error> {
        self.sink.binary_data(&self.searcher, binary_byte_offset)
    }

    pub fn begin(&mut self) -> Result<bool, S::Error> {
        self.sink.begin(&self.searcher)
    }

    pub fn finish(
        &mut self,
        byte_count: u64,
        binary_byte_offset: Option<u64>,
    ) -> Result<(), S::Error> {
        self.sink.finish(
            &self.searcher,
            &SinkFinish { byte_count, binary_byte_offset },
        )
    }

    pub fn match_by_line(&mut self, buf: &[u8]) -> Result<bool, S::Error> {
        use FastMatchResult::*;
        if self.is_line_by_line_fast() {
            match self.match_by_line_fast(buf)? {
                SwitchToSlow => self.match_by_line_slow(buf),
                Continue => Ok(true),
                Stop => Ok(false),
            }
        } else {
            self.match_by_line_slow(buf)
        }
    }

    pub fn roll(&mut self, buf: &[u8]) -> usize {
        let consumed = if self.config.max_context() == 0 {
            buf.len()
        } else {
            // It might seem like all we need to care about here is just
            // the "before context," but in order to sink the context
            // separator (when before_context==0 and after_context>0), we
            // need to know something about the position of the previous
            // line visited, even if we're at the beginning of the buffer.
            let context_start = lines::preceding(
                buf,
                self.config.line_term.as_byte(),
                self.config.max_context(),
            );
            let consumed = cmp::max(context_start, self.last_line_visited);
            consumed
        };
        self.count_lines(buf, consumed);
        self.absolute_byte_offset += consumed as u64;
        self.last_line_counted = 0;
        self.last_line_visited = 0;
        self.set_pos(buf.len() - consumed);
        consumed
    }

    pub fn detect_binary(
        &mut self,
        buf: &[u8],
        range: &Range,
    ) -> Result<bool, S::Error> {
        if self.binary_byte_offset.is_some() {
            return Ok(self.config.binary.quit_byte().is_some());
        }
        let binary_byte = match self.config.binary.0 {
            BinaryDetection::Quit(b) => b,
            BinaryDetection::Convert(b) => b,
            _ => return Ok(false),
        };
        if let Some(i) = buf[*range].find_byte(binary_byte) {
            let offset = range.start() + i;
            self.binary_byte_offset = Some(offset);
            if !self.binary_data(offset as u64)? {
                return Ok(true);
            }
            Ok(self.config.binary.quit_byte().is_some())
        } else {
            Ok(false)
        }
    }

    pub fn before_context_by_line(
        &mut self,
        buf: &[u8],
        upto: usize,
    ) -> Result<bool, S::Error> {
        if self.config.before_context == 0 {
            return Ok(true);
        }
        let range = Range::new(self.last_line_visited, upto);
        if range.is_empty() {
            return Ok(true);
        }
        let before_context_start = range.start()
            + lines::preceding(
                &buf[range],
                self.config.line_term.as_byte(),
                self.config.before_context - 1,
            );

        let range = Range::new(before_context_start, range.end());
        let mut stepper = LineStep::new(
            self.config.line_term.as_byte(),
            range.start(),
            range.end(),
        );
        while let Some(line) = stepper.next_match(buf) {
            if !self.sink_break_context(line.start())? {
                return Ok(false);
            }
            if !self.sink_before_context(buf, &line)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub fn after_context_by_line(
        &mut self,
        buf: &[u8],
        upto: usize,
    ) -> Result<bool, S::Error> {
        if self.after_context_left == 0 {
            return Ok(true);
        }
        let range = Range::new(self.last_line_visited, upto);
        let mut stepper = LineStep::new(
            self.config.line_term.as_byte(),
            range.start(),
            range.end(),
        );
        while let Some(line) = stepper.next_match(buf) {
            if !self.sink_after_context(buf, &line)? {
                return Ok(false);
            }
            if self.after_context_left == 0 {
                break;
            }
        }
        Ok(true)
    }

    pub fn other_context_by_line(
        &mut self,
        buf: &[u8],
        upto: usize,
    ) -> Result<bool, S::Error> {
        let range = Range::new(self.last_line_visited, upto);
        let mut stepper = LineStep::new(
            self.config.line_term.as_byte(),
            range.start(),
            range.end(),
        );
        while let Some(line) = stepper.next_match(buf) {
            if !self.sink_other_context(buf, &line)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn match_by_line_slow(&mut self, buf: &[u8]) -> Result<bool, S::Error> {
        debug_assert!(!self.searcher.multi_line_with_matcher(&self.matcher));

        let range = Range::new(self.pos(), buf.len());
        let mut stepper = LineStep::new(
            self.config.line_term.as_byte(),
            range.start(),
            range.end(),
        );
        while let Some(line) = stepper.next_match(buf) {
            let matched = {
                // Stripping the line terminator is necessary to prevent some
                // classes of regexes from matching the empty position *after*
                // the end of the line. For example, `(?m)^$` will match at
                // position (2, 2) in the string `a\n`.
                let slice = lines::without_terminator(
                    &buf[line],
                    self.config.line_term,
                );
                match self.matcher.shortest_match(slice) {
                    Err(err) => return Err(S::Error::error_message(err)),
                    Ok(result) => result.is_some(),
                }
            };
            self.set_pos(line.end());
            let success = matched != self.config.invert_match;
            if success {
                self.has_matched = true;
                if !self.before_context_by_line(buf, line.start())? {
                    return Ok(false);
                }
                if !self.sink_matched(buf, &line)? {
                    return Ok(false);
                }
            } else if self.after_context_left >= 1 {
                if !self.sink_after_context(buf, &line)? {
                    return Ok(false);
                }
            } else if self.config.passthru {
                if !self.sink_other_context(buf, &line)? {
                    return Ok(false);
                }
            }
            if !success && self.config.stop_on_nonmatch && self.has_matched {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn match_by_line_fast(
        &mut self,
        buf: &[u8],
    ) -> Result<FastMatchResult, S::Error> {
        debug_assert!(!self.config.passthru);
        use FastMatchResult::*;

        while !buf[self.pos()..].is_empty() {
            if self.has_matched && self.config.stop_on_nonmatch {
                return Ok(SwitchToSlow);
            }
            if self.config.invert_match {
                if !self.match_by_line_fast_invert(buf)? {
                    return Ok(Stop);
                }
            } else if let Some(line) = self.find_by_line_fast(buf)? {
                self.has_matched = true;
                if self.config.max_context() > 0 {
                    if !self.after_context_by_line(buf, line.start())? {
                        return Ok(Stop);
                    }
                    if !self.before_context_by_line(buf, line.start())? {
                        return Ok(Stop);
                    }
                }
                self.set_pos(line.end());
                if !self.sink_matched(buf, &line)? {
                    return Ok(Stop);
                }
            } else {
                break;
            }
        }
        if !self.after_context_by_line(buf, buf.len())? {
            return Ok(Stop);
        }
        self.set_pos(buf.len());
        Ok(Continue)
    }

    #[inline(always)]
    fn match_by_line_fast_invert(
        &mut self,
        buf: &[u8],
    ) -> Result<bool, S::Error> {
        assert!(self.config.invert_match);

        let invert_match = match self.find_by_line_fast(buf)? {
            None => {
                let range = Range::new(self.pos(), buf.len());
                self.set_pos(range.end());
                range
            }
            Some(line) => {
                let range = Range::new(self.pos(), line.start());
                self.set_pos(line.end());
                range
            }
        };
        if invert_match.is_empty() {
            return Ok(true);
        }
        self.has_matched = true;
        if !self.after_context_by_line(buf, invert_match.start())? {
            return Ok(false);
        }
        if !self.before_context_by_line(buf, invert_match.start())? {
            return Ok(false);
        }
        let mut stepper = LineStep::new(
            self.config.line_term.as_byte(),
            invert_match.start(),
            invert_match.end(),
        );
        while let Some(line) = stepper.next_match(buf) {
            if !self.sink_matched(buf, &line)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    #[inline(always)]
    fn find_by_line_fast(
        &self,
        buf: &[u8],
    ) -> Result<Option<Range>, S::Error> {
        debug_assert!(!self.searcher.multi_line_with_matcher(&self.matcher));
        debug_assert!(self.is_line_by_line_fast());

        let mut pos = self.pos();
        while !buf[pos..].is_empty() {
            match self.matcher.find_candidate_line(&buf[pos..]) {
                Err(err) => return Err(S::Error::error_message(err)),
                Ok(None) => return Ok(None),
                Ok(Some(LineMatchKind::Confirmed(i))) => {
                    let line = lines::locate(
                        buf,
                        self.config.line_term.as_byte(),
                        Range::zero(i).offset(pos),
                    );
                    // If we matched beyond the end of the buffer, then we
                    // don't report this as a match.
                    if line.start() == buf.len() {
                        pos = buf.len();
                        continue;
                    }
                    return Ok(Some(line));
                }
                Ok(Some(LineMatchKind::Candidate(i))) => {
                    let line = lines::locate(
                        buf,
                        self.config.line_term.as_byte(),
                        Range::zero(i).offset(pos),
                    );
                    // We need to strip the line terminator here to match the
                    // semantics of line-by-line searching. Namely, regexes
                    // like `(?m)^$` can match at the final position beyond a
                    // line terminator, which is non-sensical in line oriented
                    // matching.
                    let slice = lines::without_terminator(
                        &buf[line],
                        self.config.line_term,
                    );
                    match self.matcher.is_match(slice) {
                        Err(err) => return Err(S::Error::error_message(err)),
                        Ok(true) => return Ok(Some(line)),
                        Ok(false) => {
                            pos = line.end();
                            continue;
                        }
                    }
                }
            }
        }
        Ok(None)
    }

    #[inline(always)]
    fn sink_matched(
        &mut self,
        buf: &[u8],
        range: &Range,
    ) -> Result<bool, S::Error> {
        if self.binary && self.detect_binary(buf, range)? {
            return Ok(false);
        }
        if !self.sink_break_context(range.start())? {
            return Ok(false);
        }
        self.count_lines(buf, range.start());
        let offset = self.absolute_byte_offset + range.start() as u64;
        let linebuf = &buf[*range];
        let keepgoing = self.sink.matched(
            &self.searcher,
            &SinkMatch {
                line_term: self.config.line_term,
                bytes: linebuf,
                absolute_byte_offset: offset,
                line_number: self.line_number,
                buffer: buf,
                bytes_range_in_buffer: range.start()..range.end(),
            },
        )?;
        if !keepgoing {
            return Ok(false);
        }
        self.last_line_visited = range.end();
        self.after_context_left = self.config.after_context;
        self.has_sunk = true;
        Ok(true)
    }

    fn sink_before_context(
        &mut self,
        buf: &[u8],
        range: &Range,
    ) -> Result<bool, S::Error> {
        if self.binary && self.detect_binary(buf, range)? {
            return Ok(false);
        }
        self.count_lines(buf, range.start());
        let offset = self.absolute_byte_offset + range.start() as u64;
        let keepgoing = self.sink.context(
            &self.searcher,
            &SinkContext {
                line_term: self.config.line_term,
                bytes: &buf[*range],
                kind: SinkContextKind::Before,
                absolute_byte_offset: offset,
                line_number: self.line_number,
            },
        )?;
        if !keepgoing {
            return Ok(false);
        }
        self.last_line_visited = range.end();
        self.has_sunk = true;
        Ok(true)
    }

    fn sink_after_context(
        &mut self,
        buf: &[u8],
        range: &Range,
    ) -> Result<bool, S::Error> {
        assert!(self.after_context_left >= 1);

        if self.binary && self.detect_binary(buf, range)? {
            return Ok(false);
        }
        self.count_lines(buf, range.start());
        let offset = self.absolute_byte_offset + range.start() as u64;
        let keepgoing = self.sink.context(
            &self.searcher,
            &SinkContext {
                line_term: self.config.line_term,
                bytes: &buf[*range],
                kind: SinkContextKind::After,
                absolute_byte_offset: offset,
                line_number: self.line_number,
            },
        )?;
        if !keepgoing {
            return Ok(false);
        }
        self.last_line_visited = range.end();
        self.after_context_left -= 1;
        self.has_sunk = true;
        Ok(true)
    }

    fn sink_other_context(
        &mut self,
        buf: &[u8],
        range: &Range,
    ) -> Result<bool, S::Error> {
        if self.binary && self.detect_binary(buf, range)? {
            return Ok(false);
        }
        self.count_lines(buf, range.start());
        let offset = self.absolute_byte_offset + range.start() as u64;
        let keepgoing = self.sink.context(
            &self.searcher,
            &SinkContext {
                line_term: self.config.line_term,
                bytes: &buf[*range],
                kind: SinkContextKind::Other,
                absolute_byte_offset: offset,
                line_number: self.line_number,
            },
        )?;
        if !keepgoing {
            return Ok(false);
        }
        self.last_line_visited = range.end();
        self.has_sunk = true;
        Ok(true)
    }

    fn sink_break_context(
        &mut self,
        start_of_line: usize,
    ) -> Result<bool, S::Error> {
        let is_gap = self.last_line_visited < start_of_line;
        let any_context =
            self.config.before_context > 0 || self.config.after_context > 0;

        if !any_context || !self.has_sunk || !is_gap {
            Ok(true)
        } else {
            self.sink.context_break(&self.searcher)
        }
    }

    fn count_lines(&mut self, buf: &[u8], upto: usize) {
        if let Some(ref mut line_number) = self.line_number {
            if self.last_line_counted >= upto {
                return;
            }
            let slice = &buf[self.last_line_counted..upto];
            let count = lines::count(slice, self.config.line_term.as_byte());
            *line_number += count;
            self.last_line_counted = upto;
        }
    }

    fn is_line_by_line_fast(&self) -> bool {
        debug_assert!(!self.searcher.multi_line_with_matcher(&self.matcher));

        if self.config.passthru {
            return false;
        }
        if self.config.stop_on_nonmatch && self.has_matched {
            return false;
        }
        if let Some(line_term) = self.matcher.line_terminator() {
            if line_term == self.config.line_term {
                return true;
            }
        }
        if let Some(non_matching) = self.matcher.non_matching_bytes() {
            // If the line terminator is CRLF, we don't actually need to care
            // whether the regex can match `\r` or not. Namely, a `\r` is
            // neither necessary nor sufficient to terminate a line. A `\n` is
            // always required.
            if non_matching.contains(self.config.line_term.as_byte()) {
                return true;
            }
        }
        false
    }
}

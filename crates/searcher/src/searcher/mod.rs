use std::cell::RefCell;
use std::cmp;
use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use crate::line_buffer::{
    self, alloc_error, BufferAllocation, LineBuffer, LineBufferBuilder,
    LineBufferReader, DEFAULT_BUFFER_CAPACITY,
};
use crate::searcher::glue::{MultiLine, ReadByLine, SliceByLine};
use crate::sink::{Sink, SinkError};
use encoding_rs;
use encoding_rs_io::DecodeReaderBytesBuilder;
use grep_matcher::{LineTerminator, Match, Matcher};

pub use self::mmap::MmapChoice;

mod core;
mod glue;
mod mmap;

/// We use this type alias since we want the ergonomics of a matcher's `Match`
/// type, but in practice, we use it for arbitrary ranges, so give it a more
/// accurate name. This is only used in the searcher's internals.
type Range = Match;

/// The behavior of binary detection while searching.
///
/// Binary detection is the process of _heuristically_ identifying whether a
/// given chunk of data is binary or not, and then taking an action based on
/// the result of that heuristic. The motivation behind detecting binary data
/// is that binary data often indicates data that is undesirable to search
/// using textual patterns. Of course, there are many cases in which this isn't
/// true, which is why binary detection is disabled by default.
///
/// Unfortunately, binary detection works differently depending on the type of
/// search being executed:
///
/// 1. When performing a search using a fixed size buffer, binary detection is
///    applied to the buffer's contents as it is filled. Binary detection must
///    be applied to the buffer directly because binary files may not contain
///    line terminators, which could result in exorbitant memory usage.
/// 2. When performing a search using memory maps or by reading data off the
///    heap, then binary detection is only guaranteed to be applied to the
///    parts corresponding to a match. When `Quit` is enabled, then the first
///    few KB of the data are searched for binary data.
#[derive(Clone, Debug, Default)]
pub struct BinaryDetection(line_buffer::BinaryDetection);

impl BinaryDetection {
    /// No binary detection is performed. Data reported by the searcher may
    /// contain arbitrary bytes.
    ///
    /// This is the default.
    pub fn none() -> BinaryDetection {
        BinaryDetection(line_buffer::BinaryDetection::None)
    }

    /// Binary detection is performed by looking for the given byte.
    ///
    /// When searching is performed using a fixed size buffer, then the
    /// contents of that buffer are always searched for the presence of this
    /// byte. If it is found, then the underlying data is considered binary
    /// and the search stops as if it reached EOF.
    ///
    /// When searching is performed with the entire contents mapped into
    /// memory, then binary detection is more conservative. Namely, only a
    /// fixed sized region at the beginning of the contents are detected for
    /// binary data. As a compromise, any subsequent matching (or context)
    /// lines are also searched for binary data. If binary data is detected at
    /// any point, then the search stops as if it reached EOF.
    pub fn quit(binary_byte: u8) -> BinaryDetection {
        BinaryDetection(line_buffer::BinaryDetection::Quit(binary_byte))
    }

    /// Binary detection is performed by looking for the given byte, and
    /// replacing it with the line terminator configured on the searcher.
    /// (If the searcher is configured to use `CRLF` as the line terminator,
    /// then this byte is replaced by just `LF`.)
    ///
    /// When searching is performed using a fixed size buffer, then the
    /// contents of that buffer are always searched for the presence of this
    /// byte and replaced with the line terminator. In effect, the caller is
    /// guaranteed to never observe this byte while searching.
    ///
    /// When searching is performed with the entire contents mapped into
    /// memory, then this setting has no effect and is ignored.
    pub fn convert(binary_byte: u8) -> BinaryDetection {
        BinaryDetection(line_buffer::BinaryDetection::Convert(binary_byte))
    }

    /// If this binary detection uses the "quit" strategy, then this returns
    /// the byte that will cause a search to quit. In any other case, this
    /// returns `None`.
    pub fn quit_byte(&self) -> Option<u8> {
        match self.0 {
            line_buffer::BinaryDetection::Quit(b) => Some(b),
            _ => None,
        }
    }

    /// If this binary detection uses the "convert" strategy, then this returns
    /// the byte that will be replaced by the line terminator. In any other
    /// case, this returns `None`.
    pub fn convert_byte(&self) -> Option<u8> {
        match self.0 {
            line_buffer::BinaryDetection::Convert(b) => Some(b),
            _ => None,
        }
    }
}

/// An encoding to use when searching.
///
/// An encoding can be used to configure a
/// [`SearcherBuilder`](struct.SearchBuilder.html)
/// to transcode source data from an encoding to UTF-8 before searching.
///
/// An `Encoding` will always be cheap to clone.
#[derive(Clone, Debug)]
pub struct Encoding(&'static encoding_rs::Encoding);

impl Encoding {
    /// Create a new encoding for the specified label.
    ///
    /// The encoding label provided is mapped to an encoding via the set of
    /// available choices specified in the
    /// [Encoding Standard](https://encoding.spec.whatwg.org/#concept-encoding-get).
    /// If the given label does not correspond to a valid encoding, then this
    /// returns an error.
    pub fn new(label: &str) -> Result<Encoding, ConfigError> {
        let label = label.as_bytes();
        match encoding_rs::Encoding::for_label_no_replacement(label) {
            Some(encoding) => Ok(Encoding(encoding)),
            None => {
                Err(ConfigError::UnknownEncoding { label: label.to_vec() })
            }
        }
    }
}

/// The internal configuration of a searcher. This is shared among several
/// search related types, but is only ever written to by the SearcherBuilder.
#[derive(Clone, Debug)]
pub struct Config {
    /// The line terminator to use.
    line_term: LineTerminator,
    /// Whether to invert matching.
    invert_match: bool,
    /// The number of lines after a match to include.
    after_context: usize,
    /// The number of lines before a match to include.
    before_context: usize,
    /// Whether to enable unbounded context or not.
    passthru: bool,
    /// Whether to count line numbers.
    line_number: bool,
    /// The maximum amount of heap memory to use.
    ///
    /// When not given, no explicit limit is enforced. When set to `0`, then
    /// only the memory map search strategy is available.
    heap_limit: Option<usize>,
    /// The memory map strategy.
    mmap: MmapChoice,
    /// The binary data detection strategy.
    binary: BinaryDetection,
    /// Whether to enable matching across multiple lines.
    multi_line: bool,
    /// An encoding that, when present, causes the searcher to transcode all
    /// input from the encoding to UTF-8.
    encoding: Option<Encoding>,
    /// Whether to do automatic transcoding based on a BOM or not.
    bom_sniffing: bool,
    /// Whether to stop searching when a non-matching line is found after a match.
    stop_on_nonmatch: bool,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            line_term: LineTerminator::default(),
            invert_match: false,
            after_context: 0,
            before_context: 0,
            passthru: false,
            line_number: true,
            heap_limit: None,
            mmap: MmapChoice::default(),
            binary: BinaryDetection::default(),
            multi_line: false,
            encoding: None,
            bom_sniffing: true,
            stop_on_nonmatch: false,
        }
    }
}

impl Config {
    /// Return the maximal amount of lines needed to fulfill this
    /// configuration's context.
    ///
    /// If this returns `0`, then no context is ever needed.
    fn max_context(&self) -> usize {
        cmp::max(self.before_context, self.after_context)
    }

    /// Build a line buffer from this configuration.
    fn line_buffer(&self) -> LineBuffer {
        let mut builder = LineBufferBuilder::new();
        builder
            .line_terminator(self.line_term.as_byte())
            .binary_detection(self.binary.0);

        if let Some(limit) = self.heap_limit {
            let (capacity, additional) = if limit <= DEFAULT_BUFFER_CAPACITY {
                (limit, 0)
            } else {
                (DEFAULT_BUFFER_CAPACITY, limit - DEFAULT_BUFFER_CAPACITY)
            };
            builder
                .capacity(capacity)
                .buffer_alloc(BufferAllocation::Error(additional));
        }
        builder.build()
    }
}

/// An error that can occur when building a searcher.
///
/// This error occurs when a non-sensical configuration is present when trying
/// to construct a `Searcher` from a `SearcherBuilder`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConfigError {
    /// Indicates that the heap limit configuration prevents all possible
    /// search strategies from being used. For example, if the heap limit is
    /// set to 0 and memory map searching is disabled or unavailable.
    SearchUnavailable,
    /// Occurs when a matcher reports a line terminator that is different than
    /// the one configured in the searcher.
    MismatchedLineTerminators {
        /// The matcher's line terminator.
        matcher: LineTerminator,
        /// The searcher's line terminator.
        searcher: LineTerminator,
    },
    /// Occurs when no encoding could be found for a particular label.
    UnknownEncoding {
        /// The provided encoding label that could not be found.
        label: Vec<u8>,
    },
    /// Hints that destructuring should not be exhaustive.
    ///
    /// This enum may grow additional variants, so this makes sure clients
    /// don't count on exhaustive matching. (Otherwise, adding a new variant
    /// could break existing code.)
    #[doc(hidden)]
    __Nonexhaustive,
}

impl ::std::error::Error for ConfigError {
    fn description(&self) -> &str {
        "grep-searcher configuration error"
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            ConfigError::SearchUnavailable => {
                write!(f, "grep config error: no available searchers")
            }
            ConfigError::MismatchedLineTerminators { matcher, searcher } => {
                write!(
                    f,
                    "grep config error: mismatched line terminators, \
                     matcher has {:?} but searcher has {:?}",
                    matcher, searcher
                )
            }
            ConfigError::UnknownEncoding { ref label } => write!(
                f,
                "grep config error: unknown encoding: {}",
                String::from_utf8_lossy(label),
            ),
            _ => panic!("BUG: unexpected variant found"),
        }
    }
}

/// A builder for configuring a searcher.
///
/// A search builder permits specifying the configuration of a searcher,
/// including options like whether to invert the search or to enable multi
/// line search.
///
/// Once a searcher has been built, it is beneficial to reuse that searcher
/// for multiple searches, if possible.
#[derive(Clone, Debug)]
pub struct SearcherBuilder {
    config: Config,
}

impl Default for SearcherBuilder {
    fn default() -> SearcherBuilder {
        SearcherBuilder::new()
    }
}

impl SearcherBuilder {
    /// Create a new searcher builder with a default configuration.
    pub fn new() -> SearcherBuilder {
        SearcherBuilder { config: Config::default() }
    }

    /// Build a searcher with the given matcher.
    pub fn build(&self) -> Searcher {
        let mut config = self.config.clone();
        if config.passthru {
            config.before_context = 0;
            config.after_context = 0;
        }

        let mut decode_builder = DecodeReaderBytesBuilder::new();
        decode_builder
            .encoding(self.config.encoding.as_ref().map(|e| e.0))
            .utf8_passthru(true)
            .strip_bom(self.config.bom_sniffing)
            .bom_override(true)
            .bom_sniffing(self.config.bom_sniffing);

        Searcher {
            config: config,
            decode_builder: decode_builder,
            decode_buffer: RefCell::new(vec![0; 8 * (1 << 10)]),
            line_buffer: RefCell::new(self.config.line_buffer()),
            multi_line_buffer: RefCell::new(vec![]),
        }
    }

    /// Set the line terminator that is used by the searcher.
    ///
    /// When using a searcher, if the matcher provided has a line terminator
    /// set, then it must be the same as this one. If they aren't, building
    /// a searcher will return an error.
    ///
    /// By default, this is set to `b'\n'`.
    pub fn line_terminator(
        &mut self,
        line_term: LineTerminator,
    ) -> &mut SearcherBuilder {
        self.config.line_term = line_term;
        self
    }

    /// Whether to invert matching, whereby lines that don't match are reported
    /// instead of reporting lines that do match.
    ///
    /// By default, this is disabled.
    pub fn invert_match(&mut self, yes: bool) -> &mut SearcherBuilder {
        self.config.invert_match = yes;
        self
    }

    /// Whether to count and include line numbers with matching lines.
    ///
    /// This is enabled by default. There is a small performance penalty
    /// associated with computing line numbers, so this can be disabled when
    /// this isn't desirable.
    pub fn line_number(&mut self, yes: bool) -> &mut SearcherBuilder {
        self.config.line_number = yes;
        self
    }

    /// Whether to enable multi line search or not.
    ///
    /// When multi line search is enabled, matches *may* match across multiple
    /// lines. Conversely, when multi line search is disabled, it is impossible
    /// for any match to span more than one line.
    ///
    /// **Warning:** multi line search requires having the entire contents to
    /// search mapped in memory at once. When searching files, memory maps
    /// will be used if possible and if they are enabled, which avoids using
    /// your program's heap. However, if memory maps cannot be used (e.g.,
    /// for searching streams like `stdin` or if transcoding is necessary),
    /// then the entire contents of the stream are read on to the heap before
    /// starting the search.
    ///
    /// This is disabled by default.
    pub fn multi_line(&mut self, yes: bool) -> &mut SearcherBuilder {
        self.config.multi_line = yes;
        self
    }

    /// Whether to include a fixed number of lines after every match.
    ///
    /// When this is set to a non-zero number, then the searcher will report
    /// `line_count` contextual lines after every match.
    ///
    /// This is set to `0` by default.
    pub fn after_context(
        &mut self,
        line_count: usize,
    ) -> &mut SearcherBuilder {
        self.config.after_context = line_count;
        self
    }

    /// Whether to include a fixed number of lines before every match.
    ///
    /// When this is set to a non-zero number, then the searcher will report
    /// `line_count` contextual lines before every match.
    ///
    /// This is set to `0` by default.
    pub fn before_context(
        &mut self,
        line_count: usize,
    ) -> &mut SearcherBuilder {
        self.config.before_context = line_count;
        self
    }

    /// Whether to enable the "passthru" feature or not.
    ///
    /// When passthru is enabled, it effectively treats all non-matching lines
    /// as contextual lines. In other words, enabling this is akin to
    /// requesting an unbounded number of before and after contextual lines.
    ///
    /// When passthru mode is enabled, any `before_context` or `after_context`
    /// settings are ignored by setting them to `0`.
    ///
    /// This is disabled by default.
    pub fn passthru(&mut self, yes: bool) -> &mut SearcherBuilder {
        self.config.passthru = yes;
        self
    }

    /// Set an approximate limit on the amount of heap space used by a
    /// searcher.
    ///
    /// The heap limit is enforced in two scenarios:
    ///
    /// * When searching using a fixed size buffer, the heap limit controls
    ///   how big this buffer is allowed to be. Assuming contexts are disabled,
    ///   the minimum size of this buffer is the length (in bytes) of the
    ///   largest single line in the contents being searched. If any line
    ///   exceeds the heap limit, then an error will be returned.
    /// * When performing a multi line search, a fixed size buffer cannot be
    ///   used. Thus, the only choices are to read the entire contents on to
    ///   the heap, or use memory maps. In the former case, the heap limit set
    ///   here is enforced.
    ///
    /// If a heap limit is set to `0`, then no heap space is used. If there are
    /// no alternative strategies available for searching without heap space
    /// (e.g., memory maps are disabled), then the searcher wil return an error
    /// immediately.
    ///
    /// By default, no limit is set.
    pub fn heap_limit(
        &mut self,
        bytes: Option<usize>,
    ) -> &mut SearcherBuilder {
        self.config.heap_limit = bytes;
        self
    }

    /// Set the strategy to employ use of memory maps.
    ///
    /// Currently, there are only two strategies that can be employed:
    ///
    /// * **Automatic** - A searcher will use heuristics, including but not
    ///   limited to file size and platform, to determine whether to use memory
    ///   maps or not.
    /// * **Never** - Memory maps will never be used. If multi line search is
    ///   enabled, then the entire contents will be read on to the heap before
    ///   searching begins.
    ///
    /// The default behavior is **never**. Generally speaking, and perhaps
    /// against conventional wisdom, memory maps don't necessarily enable
    /// faster searching. For example, depending on the platform, using memory
    /// maps while searching a large directory can actually be quite a bit
    /// slower than using normal read calls because of the overhead of managing
    /// the memory maps.
    ///
    /// Memory maps can be faster in some cases however. On some platforms,
    /// when searching a very large file that *is already in memory*, it can
    /// be slightly faster to search it as a memory map instead of using
    /// normal read calls.
    ///
    /// Finally, memory maps have a somewhat complicated safety story in Rust.
    /// If you aren't sure whether enabling memory maps is worth it, then just
    /// don't bother with it.
    ///
    /// **WARNING**: If your process is searching a file backed memory map
    /// at the same time that file is truncated, then it's possible for the
    /// process to terminate with a bus error.
    pub fn memory_map(
        &mut self,
        strategy: MmapChoice,
    ) -> &mut SearcherBuilder {
        self.config.mmap = strategy;
        self
    }

    /// Set the binary detection strategy.
    ///
    /// The binary detection strategy determines not only how the searcher
    /// detects binary data, but how it responds to the presence of binary
    /// data. See the [`BinaryDetection`](struct.BinaryDetection.html) type
    /// for more information.
    ///
    /// By default, binary detection is disabled.
    pub fn binary_detection(
        &mut self,
        detection: BinaryDetection,
    ) -> &mut SearcherBuilder {
        self.config.binary = detection;
        self
    }

    /// Set the encoding used to read the source data before searching.
    ///
    /// When an encoding is provided, then the source data is _unconditionally_
    /// transcoded using the encoding, unless a BOM is present. If a BOM is
    /// present, then the encoding indicated by the BOM is used instead. If the
    /// transcoding process encounters an error, then bytes are replaced with
    /// the Unicode replacement codepoint.
    ///
    /// When no encoding is specified (the default), then BOM sniffing is
    /// used (if it's enabled, which it is, by default) to determine whether
    /// the source data is UTF-8 or UTF-16, and transcoding will be performed
    /// automatically. If no BOM could be found, then the source data is
    /// searched _as if_ it were UTF-8. However, so long as the source data is
    /// at least ASCII compatible, then it is possible for a search to produce
    /// useful results.
    pub fn encoding(
        &mut self,
        encoding: Option<Encoding>,
    ) -> &mut SearcherBuilder {
        self.config.encoding = encoding;
        self
    }

    /// Enable automatic transcoding based on BOM sniffing.
    ///
    /// When this is enabled and an explicit encoding is not set, then this
    /// searcher will try to detect the encoding of the bytes being searched
    /// by sniffing its byte-order mark (BOM). In particular, when this is
    /// enabled, UTF-16 encoded files will be searched seamlessly.
    ///
    /// When this is disabled and if an explicit encoding is not set, then
    /// the bytes from the source stream will be passed through unchanged,
    /// including its BOM, if one is present.
    ///
    /// This is enabled by default.
    pub fn bom_sniffing(&mut self, yes: bool) -> &mut SearcherBuilder {
        self.config.bom_sniffing = yes;
        self
    }

    /// Stop searching a file when a non-matching line is found after a matching one.
    ///
    /// This is useful for searching sorted files where it is expected that all
    /// the matches will be on adjacent lines.
    pub fn stop_on_nonmatch(
        &mut self,
        stop_on_nonmatch: bool,
    ) -> &mut SearcherBuilder {
        self.config.stop_on_nonmatch = stop_on_nonmatch;
        self
    }
}

/// A searcher executes searches over a haystack and writes results to a caller
/// provided sink.
///
/// Matches are detected via implementations of the `Matcher` trait, which must
/// be provided by the caller when executing a search.
///
/// When possible, a searcher should be reused.
#[derive(Clone, Debug)]
pub struct Searcher {
    /// The configuration for this searcher.
    ///
    /// We make most of these settings available to users of `Searcher` via
    /// public API methods, which can be queried in implementations of `Sink`
    /// if necessary.
    config: Config,
    /// A builder for constructing a streaming reader that transcodes source
    /// data according to either an explicitly specified encoding or via an
    /// automatically detected encoding via BOM sniffing.
    ///
    /// When no transcoding is needed, then the transcoder built will pass
    /// through the underlying bytes with no additional overhead.
    decode_builder: DecodeReaderBytesBuilder,
    /// A buffer that is used for transcoding scratch space.
    decode_buffer: RefCell<Vec<u8>>,
    /// A line buffer for use in line oriented searching.
    ///
    /// We wrap it in a RefCell to permit lending out borrows of `Searcher`
    /// to sinks. We still require a mutable borrow to execute a search, so
    /// we statically prevent callers from causing RefCell to panic at runtime
    /// due to a borrowing violation.
    line_buffer: RefCell<LineBuffer>,
    /// A buffer in which to store the contents of a reader when performing a
    /// multi line search. In particular, multi line searches cannot be
    /// performed incrementally, and need the entire haystack in memory at
    /// once.
    multi_line_buffer: RefCell<Vec<u8>>,
}

impl Searcher {
    /// Create a new searcher with a default configuration.
    ///
    /// To configure the searcher (e.g., invert matching, enable memory maps,
    /// enable contexts, etc.), use the
    /// [`SearcherBuilder`](struct.SearcherBuilder.html).
    pub fn new() -> Searcher {
        SearcherBuilder::new().build()
    }

    /// Execute a search over the file with the given path and write the
    /// results to the given sink.
    ///
    /// If memory maps are enabled and the searcher heuristically believes
    /// memory maps will help the search run faster, then this will use
    /// memory maps. For this reason, callers should prefer using this method
    /// or `search_file` over the more generic `search_reader` when possible.
    pub fn search_path<P, M, S>(
        &mut self,
        matcher: M,
        path: P,
        write_to: S,
    ) -> Result<(), S::Error>
    where
        P: AsRef<Path>,
        M: Matcher,
        S: Sink,
    {
        let path = path.as_ref();
        let file = File::open(path).map_err(S::Error::error_io)?;
        self.search_file_maybe_path(matcher, Some(path), &file, write_to)
    }

    /// Execute a search over a file and write the results to the given sink.
    ///
    /// If memory maps are enabled and the searcher heuristically believes
    /// memory maps will help the search run faster, then this will use
    /// memory maps. For this reason, callers should prefer using this method
    /// or `search_path` over the more generic `search_reader` when possible.
    pub fn search_file<M, S>(
        &mut self,
        matcher: M,
        file: &File,
        write_to: S,
    ) -> Result<(), S::Error>
    where
        M: Matcher,
        S: Sink,
    {
        self.search_file_maybe_path(matcher, None, file, write_to)
    }

    fn search_file_maybe_path<M, S>(
        &mut self,
        matcher: M,
        path: Option<&Path>,
        file: &File,
        write_to: S,
    ) -> Result<(), S::Error>
    where
        M: Matcher,
        S: Sink,
    {
        if let Some(mmap) = self.config.mmap.open(file, path) {
            log::trace!("{:?}: searching via memory map", path);
            return self.search_slice(matcher, &mmap, write_to);
        }
        // Fast path for multi-line searches of files when memory maps are
        // not enabled. This pre-allocates a buffer roughly the size of the
        // file, which isn't possible when searching an arbitrary io::Read.
        if self.multi_line_with_matcher(&matcher) {
            log::trace!(
                "{:?}: reading entire file on to heap for mulitline",
                path
            );
            self.fill_multi_line_buffer_from_file::<S>(file)?;
            log::trace!("{:?}: searching via multiline strategy", path);
            MultiLine::new(
                self,
                matcher,
                &*self.multi_line_buffer.borrow(),
                write_to,
            )
            .run()
        } else {
            log::trace!("{:?}: searching using generic reader", path);
            self.search_reader(matcher, file, write_to)
        }
    }

    /// Execute a search over any implementation of `io::Read` and write the
    /// results to the given sink.
    ///
    /// When possible, this implementation will search the reader incrementally
    /// without reading it into memory. In some cases---for example, if multi
    /// line search is enabled---an incremental search isn't possible and the
    /// given reader is consumed completely and placed on the heap before
    /// searching begins. For this reason, when multi line search is enabled,
    /// one should try to use higher level APIs (e.g., searching by file or
    /// file path) so that memory maps can be used if they are available and
    /// enabled.
    pub fn search_reader<M, R, S>(
        &mut self,
        matcher: M,
        read_from: R,
        write_to: S,
    ) -> Result<(), S::Error>
    where
        M: Matcher,
        R: io::Read,
        S: Sink,
    {
        self.check_config(&matcher).map_err(S::Error::error_config)?;

        let mut decode_buffer = self.decode_buffer.borrow_mut();
        let decoder = self
            .decode_builder
            .build_with_buffer(read_from, &mut *decode_buffer)
            .map_err(S::Error::error_io)?;

        if self.multi_line_with_matcher(&matcher) {
            log::trace!(
                "generic reader: reading everything to heap for multiline"
            );
            self.fill_multi_line_buffer_from_reader::<_, S>(decoder)?;
            log::trace!("generic reader: searching via multiline strategy");
            MultiLine::new(
                self,
                matcher,
                &*self.multi_line_buffer.borrow(),
                write_to,
            )
            .run()
        } else {
            let mut line_buffer = self.line_buffer.borrow_mut();
            let rdr = LineBufferReader::new(decoder, &mut *line_buffer);
            log::trace!("generic reader: searching via roll buffer strategy");
            ReadByLine::new(
                self,
                matcher,
                rdr,
                write_to,
                self.stop_on_nonmatch(),
            )
            .run()
        }
    }

    /// Execute a search over the given slice and write the results to the
    /// given sink.
    pub fn search_slice<M, S>(
        &mut self,
        matcher: M,
        slice: &[u8],
        write_to: S,
    ) -> Result<(), S::Error>
    where
        M: Matcher,
        S: Sink,
    {
        self.check_config(&matcher).map_err(S::Error::error_config)?;

        // We can search the slice directly, unless we need to do transcoding.
        if self.slice_needs_transcoding(slice) {
            log::trace!(
                "slice reader: needs transcoding, using generic reader"
            );
            return self.search_reader(matcher, slice, write_to);
        }
        if self.multi_line_with_matcher(&matcher) {
            log::trace!("slice reader: searching via multiline strategy");
            MultiLine::new(self, matcher, slice, write_to).run()
        } else {
            log::trace!("slice reader: searching via slice-by-line strategy");
            SliceByLine::new(self, matcher, slice, write_to).run()
        }
    }

    /// Set the binary detection method used on this searcher.
    pub fn set_binary_detection(&mut self, detection: BinaryDetection) {
        self.config.binary = detection.clone();
        self.line_buffer.borrow_mut().set_binary_detection(detection.0);
    }

    /// Check that the searcher's configuration and the matcher are consistent
    /// with each other.
    fn check_config<M: Matcher>(&self, matcher: M) -> Result<(), ConfigError> {
        if self.config.heap_limit == Some(0) && !self.config.mmap.is_enabled()
        {
            return Err(ConfigError::SearchUnavailable);
        }
        let matcher_line_term = match matcher.line_terminator() {
            None => return Ok(()),
            Some(line_term) => line_term,
        };
        if matcher_line_term != self.config.line_term {
            return Err(ConfigError::MismatchedLineTerminators {
                matcher: matcher_line_term,
                searcher: self.config.line_term,
            });
        }
        Ok(())
    }

    /// Returns true if and only if the given slice needs to be transcoded.
    fn slice_needs_transcoding(&self, slice: &[u8]) -> bool {
        self.config.encoding.is_some()
            || (self.config.bom_sniffing && slice_has_bom(slice))
    }
}

/// The following methods permit querying the configuration of a searcher.
/// These can be useful in generic implementations of
/// [`Sink`](trait.Sink.html),
/// where the output may be tailored based on how the searcher is configured.
impl Searcher {
    /// Returns the line terminator used by this searcher.
    #[inline]
    pub fn line_terminator(&self) -> LineTerminator {
        self.config.line_term
    }

    /// Returns the type of binary detection configured on this searcher.
    #[inline]
    pub fn binary_detection(&self) -> &BinaryDetection {
        &self.config.binary
    }

    /// Returns true if and only if this searcher is configured to invert its
    /// search results. That is, matching lines are lines that do **not** match
    /// the searcher's matcher.
    #[inline]
    pub fn invert_match(&self) -> bool {
        self.config.invert_match
    }

    /// Returns true if and only if this searcher is configured to count line
    /// numbers.
    #[inline]
    pub fn line_number(&self) -> bool {
        self.config.line_number
    }

    /// Returns true if and only if this searcher is configured to perform
    /// multi line search.
    #[inline]
    pub fn multi_line(&self) -> bool {
        self.config.multi_line
    }

    /// Returns true if and only if this searcher is configured to stop
    /// when in finds a non-matching line after a matching one.
    #[inline]
    pub fn stop_on_nonmatch(&self) -> bool {
        self.config.stop_on_nonmatch
    }

    /// Returns true if and only if this searcher will choose a multi-line
    /// strategy given the provided matcher.
    ///
    /// This may diverge from the result of `multi_line` in cases where the
    /// searcher has been configured to execute a search that can report
    /// matches over multiple lines, but where the matcher guarantees that it
    /// will never produce a match over multiple lines.
    pub fn multi_line_with_matcher<M: Matcher>(&self, matcher: M) -> bool {
        if !self.multi_line() {
            return false;
        }
        if let Some(line_term) = matcher.line_terminator() {
            if line_term == self.line_terminator() {
                return false;
            }
        }
        if let Some(non_matching) = matcher.non_matching_bytes() {
            // If the line terminator is CRLF, we don't actually need to care
            // whether the regex can match `\r` or not. Namely, a `\r` is
            // neither necessary nor sufficient to terminate a line. A `\n` is
            // always required.
            if non_matching.contains(self.line_terminator().as_byte()) {
                return false;
            }
        }
        true
    }

    /// Returns the number of "after" context lines to report. When context
    /// reporting is not enabled, this returns `0`.
    #[inline]
    pub fn after_context(&self) -> usize {
        self.config.after_context
    }

    /// Returns the number of "before" context lines to report. When context
    /// reporting is not enabled, this returns `0`.
    #[inline]
    pub fn before_context(&self) -> usize {
        self.config.before_context
    }

    /// Returns true if and only if the searcher has "passthru" mode enabled.
    #[inline]
    pub fn passthru(&self) -> bool {
        self.config.passthru
    }

    /// Fill the buffer for use with multi-line searching from the given file.
    /// This reads from the file until EOF or until an error occurs. If the
    /// contents exceed the configured heap limit, then an error is returned.
    fn fill_multi_line_buffer_from_file<S: Sink>(
        &self,
        file: &File,
    ) -> Result<(), S::Error> {
        assert!(self.config.multi_line);

        let mut decode_buffer = self.decode_buffer.borrow_mut();
        let mut read_from = self
            .decode_builder
            .build_with_buffer(file, &mut *decode_buffer)
            .map_err(S::Error::error_io)?;

        // If we don't have a heap limit, then we can defer to std's
        // read_to_end implementation. fill_multi_line_buffer_from_reader will
        // do this too, but since we have a File, we can be a bit smarter about
        // pre-allocating here.
        //
        // If we're transcoding, then our pre-allocation might not be exact,
        // but is probably still better than nothing.
        if self.config.heap_limit.is_none() {
            let mut buf = self.multi_line_buffer.borrow_mut();
            buf.clear();
            let cap =
                file.metadata().map(|m| m.len() as usize + 1).unwrap_or(0);
            buf.reserve(cap);
            read_from.read_to_end(&mut *buf).map_err(S::Error::error_io)?;
            return Ok(());
        }
        self.fill_multi_line_buffer_from_reader::<_, S>(read_from)
    }

    /// Fill the buffer for use with multi-line searching from the given
    /// reader. This reads from the reader until EOF or until an error occurs.
    /// If the contents exceed the configured heap limit, then an error is
    /// returned.
    fn fill_multi_line_buffer_from_reader<R: io::Read, S: Sink>(
        &self,
        mut read_from: R,
    ) -> Result<(), S::Error> {
        assert!(self.config.multi_line);

        let mut buf = self.multi_line_buffer.borrow_mut();
        buf.clear();

        // If we don't have a heap limit, then we can defer to std's
        // read_to_end implementation...
        let heap_limit = match self.config.heap_limit {
            Some(heap_limit) => heap_limit,
            None => {
                read_from
                    .read_to_end(&mut *buf)
                    .map_err(S::Error::error_io)?;
                return Ok(());
            }
        };
        if heap_limit == 0 {
            return Err(S::Error::error_io(alloc_error(heap_limit)));
        }

        // ... otherwise we need to roll our own. This is likely quite a bit
        // slower than what is optimal, but we avoid worry about memory safety
        // until there's a compelling reason to speed this up.
        buf.resize(cmp::min(DEFAULT_BUFFER_CAPACITY, heap_limit), 0);
        let mut pos = 0;
        loop {
            let nread = match read_from.read(&mut buf[pos..]) {
                Ok(nread) => nread,
                Err(ref err) if err.kind() == io::ErrorKind::Interrupted => {
                    continue;
                }
                Err(err) => return Err(S::Error::error_io(err)),
            };
            if nread == 0 {
                buf.resize(pos, 0);
                return Ok(());
            }

            pos += nread;
            if buf[pos..].is_empty() {
                let additional = heap_limit - buf.len();
                if additional == 0 {
                    return Err(S::Error::error_io(alloc_error(heap_limit)));
                }
                let limit = buf.len() + additional;
                let doubled = 2 * buf.len();
                buf.resize(cmp::min(doubled, limit), 0);
            }
        }
    }
}

/// Returns true if and only if the given slice begins with a UTF-8 or UTF-16
/// BOM.
///
/// This is used by the searcher to determine if a transcoder is necessary.
/// Otherwise, it is advantageous to search the slice directly.
fn slice_has_bom(slice: &[u8]) -> bool {
    let enc = match encoding_rs::Encoding::for_bom(slice) {
        None => return false,
        Some((enc, _)) => enc,
    };
    [encoding_rs::UTF_16LE, encoding_rs::UTF_16BE, encoding_rs::UTF_8]
        .contains(&enc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{KitchenSink, RegexMatcher};

    #[test]
    fn config_error_heap_limit() {
        let matcher = RegexMatcher::new("");
        let sink = KitchenSink::new();
        let mut searcher = SearcherBuilder::new().heap_limit(Some(0)).build();
        let res = searcher.search_slice(matcher, &[], sink);
        assert!(res.is_err());
    }

    #[test]
    fn config_error_line_terminator() {
        let mut matcher = RegexMatcher::new("");
        matcher.set_line_term(Some(LineTerminator::byte(b'z')));

        let sink = KitchenSink::new();
        let mut searcher = Searcher::new();
        let res = searcher.search_slice(matcher, &[], sink);
        assert!(res.is_err());
    }

    #[test]
    fn uft8_bom_sniffing() {
        // See: https://github.com/BurntSushi/ripgrep/issues/1638
        // ripgrep must sniff utf-8 BOM, just like it does with utf-16
        let matcher = RegexMatcher::new("foo");
        let haystack: &[u8] = &[0xef, 0xbb, 0xbf, 0x66, 0x6f, 0x6f];

        let mut sink = KitchenSink::new();
        let mut searcher = SearcherBuilder::new().build();

        let res = searcher.search_slice(matcher, haystack, &mut sink);
        assert!(res.is_ok());

        let sink_output = String::from_utf8(sink.as_bytes().to_vec()).unwrap();
        assert_eq!(sink_output, "1:0:foo\nbyte count:3\n");
    }
}

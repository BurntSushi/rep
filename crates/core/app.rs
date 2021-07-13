// This module defines the set of command line arguments that ripgrep supports,
// including some light validation.
//
// This module is purposely written in a bare-bones way, since it is included
// in ripgrep's build.rs file as a way to generate a man page and completion
// files for common shells.
//
// The only other place that ripgrep deals with clap is in src/args.rs, which
// is where we read clap's configuration from the end user's arguments and turn
// it into a ripgrep-specific configuration type that is not coupled with clap.

use clap::{self, crate_authors, crate_version, App, AppSettings};
use lazy_static::lazy_static;

const ABOUT: &str = "
ripgrep (rg) recursively searches the current directory for a regex pattern.
By default, ripgrep will respect gitignore rules and automatically skip hidden
files/directories and binary files.

Use -h for short descriptions and --help for more details.

Project home page: https://github.com/BurntSushi/ripgrep
";

const USAGE: &str = "
    rg [OPTIONS] PATTERN [PATH ...]
    rg [OPTIONS] -e PATTERN ... [PATH ...]
    rg [OPTIONS] -f PATTERNFILE ... [PATH ...]
    rg [OPTIONS] --files [PATH ...]
    rg [OPTIONS] --type-list
    command | rg [OPTIONS] PATTERN
    rg [OPTIONS] --help
    rg [OPTIONS] --version";

const TEMPLATE: &str = "\
{bin} {version}
{author}
{about}

USAGE:{usage}

ARGS:
{positionals}

OPTIONS:
{unified}";

/// Build a clap application parameterized by usage strings.
pub fn app() -> App<'static, 'static> {
    // We need to specify our version in a static because we've painted clap
    // into a corner. We've told it that every string we give it will be
    // 'static, but we need to build the version string dynamically. We can
    // fake the 'static lifetime with lazy_static.
    lazy_static! {
        static ref LONG_VERSION: String = long_version(None, true);
    }

    let mut app = App::new("ripgrep")
        .author(crate_authors!())
        .version(crate_version!())
        .long_version(LONG_VERSION.as_str())
        .about(ABOUT)
        .max_term_width(100)
        .setting(AppSettings::UnifiedHelpMessage)
        .setting(AppSettings::AllArgsOverrideSelf)
        .usage(USAGE)
        .template(TEMPLATE)
        .help_message("Prints help information. Use --help for more details.");
    for arg in all_args_and_flags() {
        app = app.arg(arg.claparg);
    }
    app
}

/// Return the "long" format of ripgrep's version string.
///
/// If a revision hash is given, then it is used. If one isn't given, then
/// the RIPGREP_BUILD_GIT_HASH env var is inspected for it. If that isn't set,
/// then a revision hash is not included in the version string returned.
///
/// If `cpu` is true, then the version string will include the compiled and
/// runtime CPU features.
pub fn long_version(revision_hash: Option<&str>, cpu: bool) -> String {
    // Do we have a git hash?
    // (Yes, if ripgrep was built on a machine with `git` installed.)
    let hash = match revision_hash.or(option_env!("RIPGREP_BUILD_GIT_HASH")) {
        None => String::new(),
        Some(githash) => format!(" (rev {})", githash),
    };
    if !cpu {
        format!("{}{}", crate_version!(), hash,)
    } else {
        let runtime = runtime_cpu_features();
        if runtime.is_empty() {
            format!(
                "{}{}\n{} (compiled)",
                crate_version!(),
                hash,
                compile_cpu_features().join(" ")
            )
        } else {
            format!(
                "{}{}\n{} (compiled)\n{} (runtime)",
                crate_version!(),
                hash,
                compile_cpu_features().join(" "),
                runtime.join(" ")
            )
        }
    }
}

/// Returns the relevant CPU features enabled at compile time.
fn compile_cpu_features() -> Vec<&'static str> {
    let mut features = vec![];
    if cfg!(feature = "simd-accel") {
        features.push("+SIMD");
    } else {
        features.push("-SIMD");
    }
    if cfg!(feature = "avx-accel") {
        features.push("+AVX");
    } else {
        features.push("-AVX");
    }
    features
}

/// Returns the relevant CPU features enabled at runtime.
#[cfg(target_arch = "x86_64")]
fn runtime_cpu_features() -> Vec<&'static str> {
    // This is kind of a dirty violation of abstraction, since it assumes
    // knowledge about what specific SIMD features are being used.

    let mut features = vec![];
    if is_x86_feature_detected!("ssse3") {
        features.push("+SIMD");
    } else {
        features.push("-SIMD");
    }
    if is_x86_feature_detected!("avx2") {
        features.push("+AVX");
    } else {
        features.push("-AVX");
    }
    features
}

/// Returns the relevant CPU features enabled at runtime.
#[cfg(not(target_arch = "x86_64"))]
fn runtime_cpu_features() -> Vec<&'static str> {
    vec![]
}

/// Arg is a light alias for a clap::Arg that is specialized to compile time
/// string literals.
type Arg = clap::Arg<'static, 'static>;

/// RGArg is a light wrapper around a clap::Arg and also contains some metadata
/// about the underlying Arg so that it can be inspected for other purposes
/// (e.g., hopefully generating a man page).
///
/// Note that this type is purposely overly constrained to ripgrep's particular
/// use of clap.
#[allow(dead_code)]
#[derive(Clone)]
pub struct RGArg {
    /// The underlying clap argument.
    claparg: Arg,
    /// The name of this argument. This is always present and is the name
    /// used in the code to find the value of an argument at runtime.
    pub name: &'static str,
    /// A short documentation string describing this argument. This string
    /// should fit on a single line and be a complete sentence.
    ///
    /// This is shown in the `-h` output.
    pub doc_short: &'static str,
    /// A longer documentation string describing this argument. This usually
    /// starts with the contents of `doc_short`. This is also usually many
    /// lines, potentially paragraphs, and may contain examples and additional
    /// prose.
    ///
    /// This is shown in the `--help` output.
    pub doc_long: &'static str,
    /// Whether this flag is hidden or not.
    ///
    /// This is typically used for uncommon flags that only serve to override
    /// other flags. For example, --no-ignore is a prominent flag that disables
    /// ripgrep's gitignore functionality, but --ignore re-enables it. Since
    /// gitignore support is enabled by default, use of the --ignore flag is
    /// somewhat niche and relegated to special cases when users make use of
    /// configuration files to set defaults.
    ///
    /// Generally, these flags should be documented in the documentation for
    /// the flag they override.
    pub hidden: bool,
    /// The type of this argument.
    pub kind: RGArgKind,
}

/// The kind of a ripgrep argument.
///
/// This can be one of three possibilities: a positional argument, a boolean
/// switch flag or a flag that accepts exactly one argument. Each variant
/// stores argument type specific data.
///
/// Note that clap supports more types of arguments than this, but we don't
/// (and probably shouldn't) use them in ripgrep.
///
/// Finally, note that we don't capture *all* state about an argument in this
/// type. Some state is only known to clap. There isn't any particular reason
/// why; the state we do capture is motivated by use cases (like generating
/// documentation).
#[derive(Clone)]
pub enum RGArgKind {
    /// A positional argument.
    Positional {
        /// The name of the value used in the `-h/--help` output. By
        /// convention, this is an all-uppercase string. e.g., `PATH` or
        /// `PATTERN`.
        value_name: &'static str,
        /// Whether an argument can be repeated multiple times or not.
        ///
        /// The only argument this applies to is PATH, where an end user can
        /// specify multiple paths for ripgrep to search.
        ///
        /// If this is disabled, then an argument can only be provided once.
        /// For example, PATTERN is one such argument. (Note that the
        /// -e/--regexp flag is distinct from the positional PATTERN argument,
        /// and it can be provided multiple times.)
        multiple: bool,
    },
    /// A boolean switch.
    Switch {
        /// The long name of a flag. This is always non-empty.
        long: &'static str,
        /// The short name of a flag. This is empty if a flag only has a long
        /// name.
        short: Option<&'static str>,
        /// Whether this switch can be provided multiple times where meaning
        /// is attached to the number of times this flag is given.
        ///
        /// Note that every switch can be provided multiple times. This
        /// particular state indicates whether all instances of a switch are
        /// relevant or not.
        ///
        /// For example, the -u/--unrestricted flag can be provided multiple
        /// times where each repeated use of it indicates more relaxing of
        /// ripgrep's filtering. Conversely, the -i/--ignore-case flag can
        /// also be provided multiple times, but it is simply considered either
        /// present or not. In these cases, -u/--unrestricted has `multiple`
        /// set to `true` while -i/--ignore-case has `multiple` set to `false`.
        multiple: bool,
    },
    /// A flag the accepts a single value.
    Flag {
        /// The long name of a flag. This is always non-empty.
        long: &'static str,
        /// The short name of a flag. This is empty if a flag only has a long
        /// name.
        short: Option<&'static str>,
        /// The name of the value used in the `-h/--help` output. By
        /// convention, this is an all-uppercase string. e.g., `PATH` or
        /// `PATTERN`.
        value_name: &'static str,
        /// Whether this flag can be provided multiple times with multiple
        /// distinct values.
        ///
        /// Note that every flag can be provided multiple times. This
        /// particular state indicates whether all instances of a flag are
        /// relevant or not.
        ///
        /// For example, the -g/--glob flag can be provided multiple times and
        /// all of its values should be interpreted by ripgrep. Conversely,
        /// while the -C/--context flag can also be provided multiple times,
        /// only its last instance is used while all previous instances are
        /// ignored. In these cases, -g/--glob has `multiple` set to `true`
        /// while -C/--context has `multiple` set to `false`.
        multiple: bool,
        /// A set of possible values for this flag. If an end user provides
        /// any value other than what's in this set, then clap will report an
        /// error.
        possible_values: Vec<&'static str>,
    },
}

impl RGArg {
    /// Create a positional argument.
    ///
    /// The `long_name` parameter is the name of the argument, e.g., `pattern`.
    /// The `value_name` parameter is a name that describes the type of
    /// argument this flag accepts. It should be in uppercase, e.g., PATH or
    /// PATTERN.
    fn positional(name: &'static str, value_name: &'static str) -> RGArg {
        RGArg {
            claparg: Arg::with_name(name).value_name(value_name),
            name,
            doc_short: "",
            doc_long: "",
            hidden: false,
            kind: RGArgKind::Positional { value_name, multiple: false },
        }
    }

    /// Create a boolean switch.
    ///
    /// The `long_name` parameter is the name of the flag, e.g., `--long-name`.
    ///
    /// All switches may be repeated an arbitrary number of times. If a switch
    /// is truly boolean, that consumers of clap's configuration should only
    /// check whether the flag is present or not. Otherwise, consumers may
    /// inspect the number of times the switch is used.
    fn switch(long_name: &'static str) -> RGArg {
        let claparg = Arg::with_name(long_name).long(long_name);
        RGArg {
            claparg,
            name: long_name,
            doc_short: "",
            doc_long: "",
            hidden: false,
            kind: RGArgKind::Switch {
                long: long_name,
                short: None,
                multiple: false,
            },
        }
    }

    /// Create a flag. A flag always accepts exactly one argument.
    ///
    /// The `long_name` parameter is the name of the flag, e.g., `--long-name`.
    /// The `value_name` parameter is a name that describes the type of
    /// argument this flag accepts. It should be in uppercase, e.g., PATH or
    /// PATTERN.
    ///
    /// All flags may be repeated an arbitrary number of times. If a flag has
    /// only one logical value, that consumers of clap's configuration should
    /// only use the last value.
    fn flag(long_name: &'static str, value_name: &'static str) -> RGArg {
        let claparg = Arg::with_name(long_name)
            .long(long_name)
            .value_name(value_name)
            .takes_value(true)
            .number_of_values(1);
        RGArg {
            claparg,
            name: long_name,
            doc_short: "",
            doc_long: "",
            hidden: false,
            kind: RGArgKind::Flag {
                long: long_name,
                short: None,
                value_name,
                multiple: false,
                possible_values: vec![],
            },
        }
    }

    /// Set the short flag name.
    ///
    /// This panics if this arg isn't a switch or a flag.
    fn short(mut self, name: &'static str) -> RGArg {
        match self.kind {
            RGArgKind::Positional { .. } => panic!("expected switch or flag"),
            RGArgKind::Switch { ref mut short, .. } => {
                *short = Some(name);
            }
            RGArgKind::Flag { ref mut short, .. } => {
                *short = Some(name);
            }
        }
        self.claparg = self.claparg.short(name);
        self
    }

    /// Set the "short" help text.
    ///
    /// This should be a single line. It is shown in the `-h` output.
    fn help(mut self, text: &'static str) -> RGArg {
        self.doc_short = text;
        self.claparg = self.claparg.help(text);
        self
    }

    /// Set the "long" help text.
    ///
    /// This should be at least a single line, usually longer. It is shown in
    /// the `--help` output.
    fn long_help(mut self, text: &'static str) -> RGArg {
        self.doc_long = text;
        self.claparg = self.claparg.long_help(text);
        self
    }

    /// Enable this argument to accept multiple values.
    ///
    /// Note that while switches and flags can always be repeated an arbitrary
    /// number of times, this particular method enables the flag to be
    /// logically repeated where each occurrence of the flag may have
    /// significance. That is, when this is disabled, then a switch is either
    /// present or not and a flag has exactly one value (the last one given).
    /// When this is enabled, then a switch has a count corresponding to the
    /// number of times it is used and a flag's value is a list of all values
    /// given.
    ///
    /// For the most part, this distinction is resolved by consumers of clap's
    /// configuration.
    fn multiple(mut self) -> RGArg {
        // Why not put `multiple` on RGArg proper? Because it's useful to
        // document it distinct for each different kind. See RGArgKind docs.
        match self.kind {
            RGArgKind::Positional { ref mut multiple, .. } => {
                *multiple = true;
            }
            RGArgKind::Switch { ref mut multiple, .. } => {
                *multiple = true;
            }
            RGArgKind::Flag { ref mut multiple, .. } => {
                *multiple = true;
            }
        }
        self.claparg = self.claparg.multiple(true);
        self
    }

    /// Hide this flag from all documentation.
    fn hidden(mut self) -> RGArg {
        self.hidden = true;
        self.claparg = self.claparg.hidden(true);
        self
    }

    /// Set the possible values for this argument. If this argument is not
    /// a flag, then this panics.
    ///
    /// If the end user provides any value other than what is given here, then
    /// clap will report an error to the user.
    ///
    /// Note that this will suppress clap's automatic output of possible values
    /// when using -h/--help, so users of this method should provide
    /// appropriate documentation for the choices in the "long" help text.
    fn possible_values(mut self, values: &[&'static str]) -> RGArg {
        match self.kind {
            RGArgKind::Positional { .. } => panic!("expected flag"),
            RGArgKind::Switch { .. } => panic!("expected flag"),
            RGArgKind::Flag { ref mut possible_values, .. } => {
                *possible_values = values.to_vec();
                self.claparg = self
                    .claparg
                    .possible_values(values)
                    .hide_possible_values(true);
            }
        }
        self
    }

    /// Add an alias to this argument.
    ///
    /// Aliases are not show in the output of -h/--help.
    fn alias(mut self, name: &'static str) -> RGArg {
        self.claparg = self.claparg.alias(name);
        self
    }

    /// Permit this flag to have values that begin with a hyphen.
    ///
    /// This panics if this arg is not a flag.
    fn allow_leading_hyphen(mut self) -> RGArg {
        match self.kind {
            RGArgKind::Positional { .. } => panic!("expected flag"),
            RGArgKind::Switch { .. } => panic!("expected flag"),
            RGArgKind::Flag { .. } => {
                self.claparg = self.claparg.allow_hyphen_values(true);
            }
        }
        self
    }

    /// Sets this argument to a required argument, unless one of the given
    /// arguments is provided.
    fn required_unless(mut self, names: &[&'static str]) -> RGArg {
        self.claparg = self.claparg.required_unless_one(names);
        self
    }

    /// Sets conflicting arguments. That is, if this argument is used whenever
    /// any of the other arguments given here are used, then clap will report
    /// an error.
    fn conflicts(mut self, names: &[&'static str]) -> RGArg {
        self.claparg = self.claparg.conflicts_with_all(names);
        self
    }

    /// Sets an overriding argument. That is, if this argument and the given
    /// argument are both provided by an end user, then the "last" one will
    /// win. ripgrep will behave as if any previous instantiations did not
    /// happen.
    fn overrides(mut self, name: &'static str) -> RGArg {
        self.claparg = self.claparg.overrides_with(name);
        self
    }

    /// Sets the default value of this argument when not specified at
    /// runtime.
    fn default_value(mut self, value: &'static str) -> RGArg {
        self.claparg = self.claparg.default_value(value);
        self
    }

    /// Sets the default value of this argument if and only if the argument
    /// given is present.
    fn default_value_if(
        mut self,
        value: &'static str,
        arg_name: &'static str,
    ) -> RGArg {
        self.claparg = self.claparg.default_value_if(arg_name, None, value);
        self
    }

    /// Indicate that any value given to this argument should be a number. If
    /// it's not a number, then clap will report an error to the end user.
    fn number(mut self) -> RGArg {
        self.claparg = self.claparg.validator(|val| {
            val.parse::<usize>().map(|_| ()).map_err(|err| err.to_string())
        });
        self
    }
}

// We add an extra space to long descriptions so that a blank line is inserted
// between flag descriptions in --help output.
macro_rules! long {
    ($lit:expr) => {
        concat!($lit, " ")
    };
}

/// Generate a sequence of all positional and flag arguments.
pub fn all_args_and_flags() -> Vec<RGArg> {
    let mut args = vec![];
    // The positional arguments must be defined first and in order.
    arg_pattern(&mut args);
    arg_path(&mut args);
    // Flags can be defined in any order, but we do it alphabetically. Note
    // that each function may define multiple flags. For example,
    // `flag_encoding` defines `--encoding` and `--no-encoding`. Most `--no`
    // flags are hidden and merely mentioned in the docs of the corresponding
    // "positive" flag.
    flag_after_context(&mut args);
    flag_auto_hybrid_regex(&mut args);
    flag_before_context(&mut args);
    flag_binary(&mut args);
    flag_block_buffered(&mut args);
    flag_byte_offset(&mut args);
    flag_case_sensitive(&mut args);
    flag_color(&mut args);
    flag_colors(&mut args);
    flag_column(&mut args);
    flag_context(&mut args);
    flag_context_separator(&mut args);
    flag_count(&mut args);
    flag_count_matches(&mut args);
    flag_crlf(&mut args);
    flag_debug(&mut args);
    flag_dfa_size_limit(&mut args);
    flag_encoding(&mut args);
    flag_engine(&mut args);
    flag_field_context_separator(&mut args);
    flag_field_match_separator(&mut args);
    flag_file(&mut args);
    flag_files(&mut args);
    flag_files_with_matches(&mut args);
    flag_files_without_match(&mut args);
    flag_fixed_strings(&mut args);
    flag_follow(&mut args);
    flag_glob(&mut args);
    flag_glob_case_insensitive(&mut args);
    flag_heading(&mut args);
    flag_hidden(&mut args);
    flag_iglob(&mut args);
    flag_ignore_case(&mut args);
    flag_ignore_file(&mut args);
    flag_ignore_file_case_insensitive(&mut args);
    flag_include_zero(&mut args);
    flag_invert_match(&mut args);
    flag_json(&mut args);
    flag_line_buffered(&mut args);
    flag_line_number(&mut args);
    flag_line_regexp(&mut args);
    flag_max_columns(&mut args);
    flag_max_columns_preview(&mut args);
    flag_max_count(&mut args);
    flag_max_depth(&mut args);
    flag_max_filesize(&mut args);
    flag_mmap(&mut args);
    flag_multiline(&mut args);
    flag_multiline_dotall(&mut args);
    flag_no_config(&mut args);
    flag_no_ignore(&mut args);
    flag_no_ignore_dot(&mut args);
    flag_no_ignore_exclude(&mut args);
    flag_no_ignore_files(&mut args);
    flag_no_ignore_global(&mut args);
    flag_no_ignore_messages(&mut args);
    flag_no_ignore_parent(&mut args);
    flag_no_ignore_vcs(&mut args);
    flag_no_messages(&mut args);
    flag_no_pcre2_unicode(&mut args);
    flag_no_require_git(&mut args);
    flag_no_unicode(&mut args);
    flag_null(&mut args);
    flag_null_data(&mut args);
    flag_one_file_system(&mut args);
    flag_only_matching(&mut args);
    flag_path_separator(&mut args);
    flag_passthru(&mut args);
    flag_pcre2(&mut args);
    flag_pcre2_version(&mut args);
    flag_pre(&mut args);
    flag_pre_glob(&mut args);
    flag_pretty(&mut args);
    flag_quiet(&mut args);
    flag_regex_size_limit(&mut args);
    flag_regexp(&mut args);
    flag_replace(&mut args);
    flag_search_zip(&mut args);
    flag_smart_case(&mut args);
    flag_sort_files(&mut args);
    flag_sort(&mut args);
    flag_sortr(&mut args);
    flag_stats(&mut args);
    flag_text(&mut args);
    flag_threads(&mut args);
    flag_trim(&mut args);
    flag_type(&mut args);
    flag_type_add(&mut args);
    flag_type_clear(&mut args);
    flag_type_list(&mut args);
    flag_type_not(&mut args);
    flag_unrestricted(&mut args);
    flag_vimgrep(&mut args);
    flag_with_filename(&mut args);
    flag_word_regexp(&mut args);
    flag_stop_on_nonmatch(&mut args);
    args
}

fn arg_pattern(args: &mut Vec<RGArg>) {
    const SHORT: &str = "A regular expression used for searching.";
    const LONG: &str = long!(
        "\
A regular expression used for searching. To match a pattern beginning with a
dash, use the -e/--regexp flag.

For example, to search for the literal '-foo', you can use this flag:

    rg -e -foo

You can also use the special '--' delimiter to indicate that no more flags
will be provided. Namely, the following is equivalent to the above:

    rg -- -foo
"
    );
    let arg = RGArg::positional("pattern", "PATTERN")
        .help(SHORT)
        .long_help(LONG)
        .required_unless(&[
            "file",
            "files",
            "regexp",
            "type-list",
            "pcre2-version",
        ]);
    args.push(arg);
}

fn arg_path(args: &mut Vec<RGArg>) {
    const SHORT: &str = "A file or directory to search.";
    const LONG: &str = long!(
        "\
A file or directory to search. Directories are searched recursively. File \
paths specified on the command line override glob and ignore rules. \
"
    );
    let arg = RGArg::positional("path", "PATH")
        .help(SHORT)
        .long_help(LONG)
        .multiple();
    args.push(arg);
}

fn flag_after_context(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Show NUM lines after each match.";
    const LONG: &str = long!(
        "\
Show NUM lines after each match.

This overrides the --context and --passthru flags.
"
    );
    let arg = RGArg::flag("after-context", "NUM")
        .short("A")
        .help(SHORT)
        .long_help(LONG)
        .number()
        .overrides("passthru")
        .overrides("context");
    args.push(arg);
}

fn flag_auto_hybrid_regex(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Dynamically use PCRE2 if necessary.";
    const LONG: &str = long!(
        "\
DEPRECATED. Use --engine instead.

When this flag is used, ripgrep will dynamically choose between supported regex
engines depending on the features used in a pattern. When ripgrep chooses a
regex engine, it applies that choice for every regex provided to ripgrep (e.g.,
via multiple -e/--regexp or -f/--file flags).

As an example of how this flag might behave, ripgrep will attempt to use
its default finite automata based regex engine whenever the pattern can be
successfully compiled with that regex engine. If PCRE2 is enabled and if the
pattern given could not be compiled with the default regex engine, then PCRE2
will be automatically used for searching. If PCRE2 isn't available, then this
flag has no effect because there is only one regex engine to choose from.

In the future, ripgrep may adjust its heuristics for how it decides which
regex engine to use. In general, the heuristics will be limited to a static
analysis of the patterns, and not to any specific runtime behavior observed
while searching files.

The primary downside of using this flag is that it may not always be obvious
which regex engine ripgrep uses, and thus, the match semantics or performance
profile of ripgrep may subtly and unexpectedly change. However, in many cases,
all regex engines will agree on what constitutes a match and it can be nice
to transparently support more advanced regex features like look-around and
backreferences without explicitly needing to enable them.

This flag can be disabled with --no-auto-hybrid-regex.
"
    );
    let arg = RGArg::switch("auto-hybrid-regex")
        .help(SHORT)
        .long_help(LONG)
        .overrides("no-auto-hybrid-regex")
        .overrides("pcre2")
        .overrides("no-pcre2")
        .overrides("engine");
    args.push(arg);

    let arg = RGArg::switch("no-auto-hybrid-regex")
        .hidden()
        .overrides("auto-hybrid-regex")
        .overrides("pcre2")
        .overrides("no-pcre2")
        .overrides("engine");
    args.push(arg);
}

fn flag_before_context(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Show NUM lines before each match.";
    const LONG: &str = long!(
        "\
Show NUM lines before each match.

This overrides the --context and --passthru flags.
"
    );
    let arg = RGArg::flag("before-context", "NUM")
        .short("B")
        .help(SHORT)
        .long_help(LONG)
        .number()
        .overrides("passthru")
        .overrides("context");
    args.push(arg);
}

fn flag_binary(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Search binary files.";
    const LONG: &str = long!(
        "\
Enabling this flag will cause ripgrep to search binary files. By default,
ripgrep attempts to automatically skip binary files in order to improve the
relevance of results and make the search faster.

Binary files are heuristically detected based on whether they contain a NUL
byte or not. By default (without this flag set), once a NUL byte is seen,
ripgrep will stop searching the file. Usually, NUL bytes occur in the beginning
of most binary files. If a NUL byte occurs after a match, then ripgrep will
still stop searching the rest of the file, but a warning will be printed.

In contrast, when this flag is provided, ripgrep will continue searching a file
even if a NUL byte is found. In particular, if a NUL byte is found then ripgrep
will continue searching until either a match is found or the end of the file is
reached, whichever comes sooner. If a match is found, then ripgrep will stop
and print a warning saying that the search stopped prematurely.

If you want ripgrep to search a file without any special NUL byte handling at
all (and potentially print binary data to stdout), then you should use the
'-a/--text' flag.

The '--binary' flag is a flag for controlling ripgrep's automatic filtering
mechanism. As such, it does not need to be used when searching a file
explicitly or when searching stdin. That is, it is only applicable when
recursively searching a directory.

Note that when the '-u/--unrestricted' flag is provided for a third time, then
this flag is automatically enabled.

This flag can be disabled with '--no-binary'. It overrides the '-a/--text'
flag.
"
    );
    let arg = RGArg::switch("binary")
        .help(SHORT)
        .long_help(LONG)
        .overrides("no-binary")
        .overrides("text")
        .overrides("no-text");
    args.push(arg);

    let arg = RGArg::switch("no-binary")
        .hidden()
        .overrides("binary")
        .overrides("text")
        .overrides("no-text");
    args.push(arg);
}

fn flag_block_buffered(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Force block buffering.";
    const LONG: &str = long!(
        "\
When enabled, ripgrep will use block buffering. That is, whenever a matching
line is found, it will be written to an in-memory buffer and will not be
written to stdout until the buffer reaches a certain size. This is the default
when ripgrep's stdout is redirected to a pipeline or a file. When ripgrep's
stdout is connected to a terminal, line buffering will be used. Forcing block
buffering can be useful when dumping a large amount of contents to a terminal.

Forceful block buffering can be disabled with --no-block-buffered. Note that
using --no-block-buffered causes ripgrep to revert to its default behavior of
automatically detecting the buffering strategy. To force line buffering, use
the --line-buffered flag.
"
    );
    let arg = RGArg::switch("block-buffered")
        .help(SHORT)
        .long_help(LONG)
        .overrides("no-block-buffered")
        .overrides("line-buffered")
        .overrides("no-line-buffered");
    args.push(arg);

    let arg = RGArg::switch("no-block-buffered")
        .hidden()
        .overrides("block-buffered")
        .overrides("line-buffered")
        .overrides("no-line-buffered");
    args.push(arg);
}

fn flag_byte_offset(args: &mut Vec<RGArg>) {
    const SHORT: &str =
        "Print the 0-based byte offset for each matching line.";
    const LONG: &str = long!(
        "\
Print the 0-based byte offset within the input file before each line of output.
If -o (--only-matching) is specified, print the offset of the matching part
itself.

If ripgrep does transcoding, then the byte offset is in terms of the the result
of transcoding and not the original data. This applies similarly to another
transformation on the source, such as decompression or a --pre filter. Note
that when the PCRE2 regex engine is used, then UTF-8 transcoding is done by
default.
"
    );
    let arg =
        RGArg::switch("byte-offset").short("b").help(SHORT).long_help(LONG);
    args.push(arg);
}

fn flag_case_sensitive(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Search case sensitively (default).";
    const LONG: &str = long!(
        "\
Search case sensitively.

This overrides the -i/--ignore-case and -S/--smart-case flags.
"
    );
    let arg = RGArg::switch("case-sensitive")
        .short("s")
        .help(SHORT)
        .long_help(LONG)
        .overrides("ignore-case")
        .overrides("smart-case");
    args.push(arg);
}

fn flag_color(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Controls when to use color.";
    const LONG: &str = long!(
        "\
This flag controls when to use colors. The default setting is 'auto', which
means ripgrep will try to guess when to use colors. For example, if ripgrep is
printing to a terminal, then it will use colors, but if it is redirected to a
file or a pipe, then it will suppress color output. ripgrep will suppress color
output in some other circumstances as well. For example, if the TERM
environment variable is not set or set to 'dumb', then ripgrep will not use
colors.

The possible values for this flag are:

    never    Colors will never be used.
    auto     The default. ripgrep tries to be smart.
    always   Colors will always be used regardless of where output is sent.
    ansi     Like 'always', but emits ANSI escapes (even in a Windows console).

When the --vimgrep flag is given to ripgrep, then the default value for the
--color flag changes to 'never'.
"
    );
    let arg = RGArg::flag("color", "WHEN")
        .help(SHORT)
        .long_help(LONG)
        .possible_values(&["never", "auto", "always", "ansi"])
        .default_value_if("never", "vimgrep");
    args.push(arg);
}

fn flag_colors(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Configure color settings and styles.";
    const LONG: &str = long!(
        "\
This flag specifies color settings for use in the output. This flag may be
provided multiple times. Settings are applied iteratively. Colors are limited
to one of eight choices: red, blue, green, cyan, magenta, yellow, white and
black. Styles are limited to nobold, bold, nointense, intense, nounderline
or underline.

The format of the flag is '{type}:{attribute}:{value}'. '{type}' should be
one of path, line, column or match. '{attribute}' can be fg, bg or style.
'{value}' is either a color (for fg and bg) or a text style. A special format,
'{type}:none', will clear all color settings for '{type}'.

For example, the following command will change the match color to magenta and
the background color for line numbers to yellow:

    rg --colors 'match:fg:magenta' --colors 'line:bg:yellow' foo.

Extended colors can be used for '{value}' when the terminal supports ANSI color
sequences. These are specified as either 'x' (256-color) or 'x,x,x' (24-bit
truecolor) where x is a number between 0 and 255 inclusive. x may be given as
a normal decimal number or a hexadecimal number, which is prefixed by `0x`.

For example, the following command will change the match background color to
that represented by the rgb value (0,128,255):

    rg --colors 'match:bg:0,128,255'

or, equivalently,

    rg --colors 'match:bg:0x0,0x80,0xFF'

Note that the the intense and nointense style flags will have no effect when
used alongside these extended color codes.
"
    );
    let arg = RGArg::flag("colors", "COLOR_SPEC")
        .help(SHORT)
        .long_help(LONG)
        .multiple();
    args.push(arg);
}

fn flag_column(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Show column numbers.";
    const LONG: &str = long!(
        "\
Show column numbers (1-based). This only shows the column numbers for the first
match on each line. This does not try to account for Unicode. One byte is equal
to one column. This implies --line-number.

This flag can be disabled with --no-column.
"
    );
    let arg = RGArg::switch("column")
        .help(SHORT)
        .long_help(LONG)
        .overrides("no-column");
    args.push(arg);

    let arg = RGArg::switch("no-column").hidden().overrides("column");
    args.push(arg);
}

fn flag_context(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Show NUM lines before and after each match.";
    const LONG: &str = long!(
        "\
Show NUM lines before and after each match. This is equivalent to providing
both the -B/--before-context and -A/--after-context flags with the same value.

This overrides both the -B/--before-context and -A/--after-context flags,
in addition to the --passthru flag.
"
    );
    let arg = RGArg::flag("context", "NUM")
        .short("C")
        .help(SHORT)
        .long_help(LONG)
        .number()
        .overrides("passthru")
        .overrides("before-context")
        .overrides("after-context");
    args.push(arg);
}

fn flag_context_separator(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Set the context separator string.";
    const LONG: &str = long!(
        "\
The string used to separate non-contiguous context lines in the output. This
is only used when one of the context flags is used (-A, -B or -C). Escape
sequences like \\x7F or \\t may be used. The default value is --.

When the context separator is set to an empty string, then a line break
is still inserted. To completely disable context separators, use the
--no-context-separator flag.
"
    );

    let arg = RGArg::flag("context-separator", "SEPARATOR")
        .help(SHORT)
        .long_help(LONG)
        .overrides("no-context-separator");
    args.push(arg);

    let arg = RGArg::switch("no-context-separator")
        .hidden()
        .overrides("context-separator");
    args.push(arg);
}

fn flag_count(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Only show the count of matching lines for each file.";
    const LONG: &str = long!(
        "\
This flag suppresses normal output and shows the number of lines that match
the given patterns for each file searched. Each file containing a match has its
path and count printed on each line. Note that this reports the number of lines
that match and not the total number of matches, unless -U/--multiline is
enabled. In multiline mode, --count is equivalent to --count-matches.

If only one file is given to ripgrep, then only the count is printed if there
is a match. The --with-filename flag can be used to force printing the file
path in this case. If you need a count to be printed regardless of whether
there is a match, then use --include-zero.

This overrides the --count-matches flag. Note that when --count is combined
with --only-matching, then ripgrep behaves as if --count-matches was given.
"
    );
    let arg = RGArg::switch("count")
        .short("c")
        .help(SHORT)
        .long_help(LONG)
        .overrides("count-matches");
    args.push(arg);
}

fn flag_count_matches(args: &mut Vec<RGArg>) {
    const SHORT: &str =
        "Only show the count of individual matches for each file.";
    const LONG: &str = long!(
        "\
This flag suppresses normal output and shows the number of individual
matches of the given patterns for each file searched. Each file
containing matches has its path and match count printed on each line.
Note that this reports the total number of individual matches and not
the number of lines that match.

If only one file is given to ripgrep, then only the count is printed if there
is a match. The --with-filename flag can be used to force printing the file
path in this case.

This overrides the --count flag. Note that when --count is combined with
--only-matching, then ripgrep behaves as if --count-matches was given.
"
    );
    let arg = RGArg::switch("count-matches")
        .help(SHORT)
        .long_help(LONG)
        .overrides("count");
    args.push(arg);
}

fn flag_crlf(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Support CRLF line terminators (useful on Windows).";
    const LONG: &str = long!(
        "\
When enabled, ripgrep will treat CRLF ('\\r\\n') as a line terminator instead
of just '\\n'.

Principally, this permits '$' in regex patterns to match just before CRLF
instead of just before LF. The underlying regex engine may not support this
natively, so ripgrep will translate all instances of '$' to '(?:\\r??$)'. This
may produce slightly different than desired match offsets. It is intended as a
work-around until the regex engine supports this natively.

CRLF support can be disabled with --no-crlf.
"
    );
    let arg = RGArg::switch("crlf")
        .help(SHORT)
        .long_help(LONG)
        .overrides("no-crlf")
        .overrides("null-data");
    args.push(arg);

    let arg = RGArg::switch("no-crlf").hidden().overrides("crlf");
    args.push(arg);
}

fn flag_debug(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Show debug messages.";
    const LONG: &str = long!(
        "\
Show debug messages. Please use this when filing a bug report.

The --debug flag is generally useful for figuring out why ripgrep skipped
searching a particular file. The debug messages should mention all files
skipped and why they were skipped.

To get even more debug output, use the --trace flag, which implies --debug
along with additional trace data. With --trace, the output could be quite
large and is generally more useful for development.
"
    );
    let arg = RGArg::switch("debug").help(SHORT).long_help(LONG);
    args.push(arg);

    let arg = RGArg::switch("trace").hidden().overrides("debug");
    args.push(arg);
}

fn flag_dfa_size_limit(args: &mut Vec<RGArg>) {
    const SHORT: &str = "The upper size limit of the regex DFA.";
    const LONG: &str = long!(
        "\
The upper size limit of the regex DFA. The default limit is 10M. This should
only be changed on very large regex inputs where the (slower) fallback regex
engine may otherwise be used if the limit is reached.

The argument accepts the same size suffixes as allowed in with the
--max-filesize flag.
"
    );
    let arg = RGArg::flag("dfa-size-limit", "NUM+SUFFIX?")
        .help(SHORT)
        .long_help(LONG);
    args.push(arg);
}

fn flag_encoding(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Specify the text encoding of files to search.";
    const LONG: &str = long!(
        "\
Specify the text encoding that ripgrep will use on all files searched. The
default value is 'auto', which will cause ripgrep to do a best effort automatic
detection of encoding on a per-file basis. Automatic detection in this case
only applies to files that begin with a UTF-8 or UTF-16 byte-order mark (BOM).
No other automatic detection is performed. One can also specify 'none' which
will then completely disable BOM sniffing and always result in searching the
raw bytes, including a BOM if it's present, regardless of its encoding.

Other supported values can be found in the list of labels here:
https://encoding.spec.whatwg.org/#concept-encoding-get

For more details on encoding and how ripgrep deals with it, see GUIDE.md.

This flag can be disabled with --no-encoding.
"
    );
    let arg = RGArg::flag("encoding", "ENCODING")
        .short("E")
        .help(SHORT)
        .long_help(LONG);
    args.push(arg);

    let arg = RGArg::switch("no-encoding").hidden().overrides("encoding");
    args.push(arg);
}

fn flag_engine(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Specify which regexp engine to use.";
    const LONG: &str = long!(
        "\
Specify which regular expression engine to use. When you choose a regex engine,
it applies that choice for every regex provided to ripgrep (e.g., via multiple
-e/--regexp or -f/--file flags).

Accepted values are 'default', 'pcre2', or 'auto'.

The default value is 'default', which is the fastest and should be good for
most use cases. The 'pcre2' engine is generally useful when you want to use
features such as look-around or backreferences. 'auto' will dynamically choose
between supported regex engines depending on the features used in a pattern on
a best effort basis.

Note that the 'pcre2' engine is an optional ripgrep feature. If PCRE2 wasn't
included in your build of ripgrep, then using this flag will result in ripgrep
printing an error message and exiting.

This overrides previous uses of --pcre2 and --auto-hybrid-regex flags.
"
    );
    let arg = RGArg::flag("engine", "ENGINE")
        .help(SHORT)
        .long_help(LONG)
        .possible_values(&["default", "pcre2", "auto"])
        .default_value("default")
        .overrides("pcre2")
        .overrides("no-pcre2")
        .overrides("auto-hybrid-regex")
        .overrides("no-auto-hybrid-regex");
    args.push(arg);
}

fn flag_field_context_separator(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Set the field context separator.";
    const LONG: &str = long!(
        "\
Set the field context separator, which is used to delimit file paths, line
numbers, columns and the context itself, when printing contextual lines. The
separator may be any number of bytes, including zero. Escape sequences like
\\x7F or \\t may be used. The default value is -.
"
    );
    let arg = RGArg::flag("field-context-separator", "SEPARATOR")
        .help(SHORT)
        .long_help(LONG);
    args.push(arg);
}

fn flag_field_match_separator(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Set the match separator.";
    const LONG: &str = long!(
        "\
Set the field match separator, which is used to delimit file paths, line
numbers, columns and the match itself. The separator may be any number of
bytes, including zero. Escape sequences like \\x7F or \\t may be used. The
default value is -.
"
    );
    let arg = RGArg::flag("field-match-separator", "SEPARATOR")
        .help(SHORT)
        .long_help(LONG);
    args.push(arg);
}

fn flag_file(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Search for patterns from the given file.";
    const LONG: &str = long!(
        "\
Search for patterns from the given file, with one pattern per line. When this
flag is used multiple times or in combination with the -e/--regexp flag,
then all patterns provided are searched. Empty pattern lines will match all
input lines, and the newline is not counted as part of the pattern.

A line is printed if and only if it matches at least one of the patterns.
"
    );
    let arg = RGArg::flag("file", "PATTERNFILE")
        .short("f")
        .help(SHORT)
        .long_help(LONG)
        .multiple()
        .allow_leading_hyphen();
    args.push(arg);
}

fn flag_files(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Print each file that would be searched.";
    const LONG: &str = long!(
        "\
Print each file that would be searched without actually performing the search.
This is useful to determine whether a particular file is being searched or not.
"
    );
    let arg = RGArg::switch("files")
        .help(SHORT)
        .long_help(LONG)
        // This also technically conflicts with pattern, but the first file
        // path will actually be in pattern.
        .conflicts(&["file", "regexp", "type-list"]);
    args.push(arg);
}

fn flag_files_with_matches(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Print the paths with at least one match.";
    const LONG: &str = long!(
        "\
Print the paths with at least one match and suppress match contents.

This overrides --files-without-match.
"
    );
    let arg = RGArg::switch("files-with-matches")
        .short("l")
        .help(SHORT)
        .long_help(LONG)
        .overrides("files-without-match");
    args.push(arg);
}

fn flag_files_without_match(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Print the paths that contain zero matches.";
    const LONG: &str = long!(
        "\
Print the paths that contain zero matches and suppress match contents. This
inverts/negates the --files-with-matches flag.

This overrides --files-with-matches.
"
    );
    let arg = RGArg::switch("files-without-match")
        .help(SHORT)
        .long_help(LONG)
        .overrides("files-with-matches");
    args.push(arg);
}

fn flag_fixed_strings(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Treat the pattern as a literal string.";
    const LONG: &str = long!(
        "\
Treat the pattern as a literal string instead of a regular expression. When
this flag is used, special regular expression meta characters such as .(){}*+
do not need to be escaped.

This flag can be disabled with --no-fixed-strings.
"
    );
    let arg = RGArg::switch("fixed-strings")
        .short("F")
        .help(SHORT)
        .long_help(LONG)
        .overrides("no-fixed-strings");
    args.push(arg);

    let arg =
        RGArg::switch("no-fixed-strings").hidden().overrides("fixed-strings");
    args.push(arg);
}

fn flag_follow(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Follow symbolic links.";
    const LONG: &str = long!(
        "\
When this flag is enabled, ripgrep will follow symbolic links while traversing
directories. This is disabled by default. Note that ripgrep will check for
symbolic link loops and report errors if it finds one.

This flag can be disabled with --no-follow.
"
    );
    let arg = RGArg::switch("follow")
        .short("L")
        .help(SHORT)
        .long_help(LONG)
        .overrides("no-follow");
    args.push(arg);

    let arg = RGArg::switch("no-follow").hidden().overrides("follow");
    args.push(arg);
}

fn flag_glob(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Include or exclude files.";
    const LONG: &str = long!(
        "\
Include or exclude files and directories for searching that match the given
glob. This always overrides any other ignore logic. Multiple glob flags may be
used. Globbing rules match .gitignore globs. Precede a glob with a ! to exclude
it. If multiple globs match a file or directory, the glob given later in the
command line takes precedence.

As an extension, globs support specifying alternatives: *-g ab{c,d}* is
equivalent to *-g abc -g abd*. Empty alternatives like *-g ab{,c}* are not
currently supported. Note that this syntax extension is also currently enabled
in gitignore files, even though this syntax isn't supported by git itself.
ripgrep may disable this syntax extension in gitignore files, but it will
always remain available via the -g/--glob flag.

When this flag is set, every file and directory is applied to it to test for
a match. So for example, if you only want to search in a particular directory
'foo', then *-g foo* is incorrect because 'foo/bar' does not match the glob
'foo'. Instead, you should use *-g 'foo/**'*.
"
    );
    let arg = RGArg::flag("glob", "GLOB")
        .short("g")
        .help(SHORT)
        .long_help(LONG)
        .multiple()
        .allow_leading_hyphen();
    args.push(arg);
}

fn flag_glob_case_insensitive(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Process all glob patterns case insensitively.";
    const LONG: &str = long!(
        "\
Process glob patterns given with the -g/--glob flag case insensitively. This
effectively treats --glob as --iglob.

This flag can be disabled with the --no-glob-case-insensitive flag.
"
    );
    let arg = RGArg::switch("glob-case-insensitive")
        .help(SHORT)
        .long_help(LONG)
        .overrides("no-glob-case-insensitive");
    args.push(arg);

    let arg = RGArg::switch("no-glob-case-insensitive")
        .hidden()
        .overrides("glob-case-insensitive");
    args.push(arg);
}

fn flag_heading(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Print matches grouped by each file.";
    const LONG: &str = long!(
        "\
This flag prints the file path above clusters of matches from each file instead
of printing the file path as a prefix for each matched line. This is the
default mode when printing to a terminal.

This overrides the --no-heading flag.
"
    );
    let arg = RGArg::switch("heading")
        .help(SHORT)
        .long_help(LONG)
        .overrides("no-heading");
    args.push(arg);

    const NO_SHORT: &str = "Don't group matches by each file.";
    const NO_LONG: &str = long!(
        "\
Don't group matches by each file. If --no-heading is provided in addition to
the -H/--with-filename flag, then file paths will be printed as a prefix for
every matched line. This is the default mode when not printing to a terminal.

This overrides the --heading flag.
"
    );
    let arg = RGArg::switch("no-heading")
        .help(NO_SHORT)
        .long_help(NO_LONG)
        .overrides("heading");
    args.push(arg);
}

fn flag_hidden(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Search hidden files and directories.";
    const LONG: &str = long!(
        "\
Search hidden files and directories. By default, hidden files and directories
are skipped. Note that if a hidden file or a directory is whitelisted in an
ignore file, then it will be searched even if this flag isn't provided.

A file or directory is considered hidden if its base name starts with a dot
character ('.'). On operating systems which support a `hidden` file attribute,
like Windows, files with this attribute are also considered hidden.

This flag can be disabled with --no-hidden.
"
    );
    let arg = RGArg::switch("hidden")
        .short(".")
        .help(SHORT)
        .long_help(LONG)
        .overrides("no-hidden");
    args.push(arg);

    let arg = RGArg::switch("no-hidden").hidden().overrides("hidden");
    args.push(arg);
}

fn flag_iglob(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Include or exclude files case insensitively.";
    const LONG: &str = long!(
        "\
Include or exclude files and directories for searching that match the given
glob. This always overrides any other ignore logic. Multiple glob flags may be
used. Globbing rules match .gitignore globs. Precede a glob with a ! to exclude
it. Globs are matched case insensitively.
"
    );
    let arg = RGArg::flag("iglob", "GLOB")
        .help(SHORT)
        .long_help(LONG)
        .multiple()
        .allow_leading_hyphen();
    args.push(arg);
}

fn flag_ignore_case(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Case insensitive search.";
    const LONG: &str = long!(
        "\
When this flag is provided, the given patterns will be searched case
insensitively. The case insensitivity rules used by ripgrep conform to
Unicode's \"simple\" case folding rules.

This flag overrides -s/--case-sensitive and -S/--smart-case.
"
    );
    let arg = RGArg::switch("ignore-case")
        .short("i")
        .help(SHORT)
        .long_help(LONG)
        .overrides("case-sensitive")
        .overrides("smart-case");
    args.push(arg);
}

fn flag_ignore_file(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Specify additional ignore files.";
    const LONG: &str = long!(
        "\
Specifies a path to one or more .gitignore format rules files. These patterns
are applied after the patterns found in .gitignore and .ignore are applied
and are matched relative to the current working directory. Multiple additional
ignore files can be specified by using the --ignore-file flag several times.
When specifying multiple ignore files, earlier files have lower precedence
than later files.

If you are looking for a way to include or exclude files and directories
directly on the command line, then used -g instead.
"
    );
    let arg = RGArg::flag("ignore-file", "PATH")
        .help(SHORT)
        .long_help(LONG)
        .multiple()
        .allow_leading_hyphen();
    args.push(arg);
}

fn flag_ignore_file_case_insensitive(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Process ignore files case insensitively.";
    const LONG: &str = long!(
        "\
Process ignore files (.gitignore, .ignore, etc.) case insensitively. Note that
this comes with a performance penalty and is most useful on case insensitive
file systems (such as Windows).

This flag can be disabled with the --no-ignore-file-case-insensitive flag.
"
    );
    let arg = RGArg::switch("ignore-file-case-insensitive")
        .help(SHORT)
        .long_help(LONG)
        .overrides("no-ignore-file-case-insensitive");
    args.push(arg);

    let arg = RGArg::switch("no-ignore-file-case-insensitive")
        .hidden()
        .overrides("ignore-file-case-insensitive");
    args.push(arg);
}

fn flag_include_zero(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Include files with zero matches in summary";
    const LONG: &str = long!(
        "\
When used with --count or --count-matches, print the number of matches for
each file even if there were zero matches. This is disabled by default but can
be enabled to make ripgrep behave more like grep.
"
    );
    let arg = RGArg::switch("include-zero").help(SHORT).long_help(LONG);
    args.push(arg);
}

fn flag_invert_match(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Invert matching.";
    const LONG: &str = long!(
        "\
Invert matching. Show lines that do not match the given patterns.
"
    );
    let arg =
        RGArg::switch("invert-match").short("v").help(SHORT).long_help(LONG);
    args.push(arg);
}

fn flag_json(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Show search results in a JSON Lines format.";
    const LONG: &str = long!(
        "\
Enable printing results in a JSON Lines format.

When this flag is provided, ripgrep will emit a sequence of messages, each
encoded as a JSON object, where there are five different message types:

**begin** - A message that indicates a file is being searched and contains at
least one match.

**end** - A message the indicates a file is done being searched. This message
also include summary statistics about the search for a particular file.

**match** - A message that indicates a match was found. This includes the text
and offsets of the match.

**context** - A message that indicates a contextual line was found. This
includes the text of the line, along with any match information if the search
was inverted.

**summary** - The final message emitted by ripgrep that contains summary
statistics about the search across all files.

Since file paths or the contents of files are not guaranteed to be valid UTF-8
and JSON itself must be representable by a Unicode encoding, ripgrep will emit
all data elements as objects with one of two keys: 'text' or 'bytes'. 'text' is
a normal JSON string when the data is valid UTF-8 while 'bytes' is the base64
encoded contents of the data.

The JSON Lines format is only supported for showing search results. It cannot
be used with other flags that emit other types of output, such as --files,
--files-with-matches, --files-without-match, --count or --count-matches.
ripgrep will report an error if any of the aforementioned flags are used in
concert with --json.

Other flags that control aspects of the standard output such as
--only-matching, --heading, --replace, --max-columns, etc., have no effect
when --json is set.

A more complete description of the JSON format used can be found here:
https://docs.rs/grep-printer/*/grep_printer/struct.JSON.html

The JSON Lines format can be disabled with --no-json.
"
    );
    let arg = RGArg::switch("json")
        .help(SHORT)
        .long_help(LONG)
        .overrides("no-json")
        .conflicts(&[
            "count",
            "count-matches",
            "files",
            "files-with-matches",
            "files-without-match",
        ]);
    args.push(arg);

    let arg = RGArg::switch("no-json").hidden().overrides("json");
    args.push(arg);
}

fn flag_line_buffered(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Force line buffering.";
    const LONG: &str = long!(
        "\
When enabled, ripgrep will use line buffering. That is, whenever a matching
line is found, it will be flushed to stdout immediately. This is the default
when ripgrep's stdout is connected to a terminal, but otherwise, ripgrep will
use block buffering, which is typically faster. This flag forces ripgrep to
use line buffering even if it would otherwise use block buffering. This is
typically useful in shell pipelines, e.g.,
'tail -f something.log | rg foo --line-buffered | rg bar'.

Forceful line buffering can be disabled with --no-line-buffered. Note that
using --no-line-buffered causes ripgrep to revert to its default behavior of
automatically detecting the buffering strategy. To force block buffering, use
the --block-buffered flag.
"
    );
    let arg = RGArg::switch("line-buffered")
        .help(SHORT)
        .long_help(LONG)
        .overrides("no-line-buffered")
        .overrides("block-buffered")
        .overrides("no-block-buffered");
    args.push(arg);

    let arg = RGArg::switch("no-line-buffered")
        .hidden()
        .overrides("line-buffered")
        .overrides("block-buffered")
        .overrides("no-block-buffered");
    args.push(arg);
}

fn flag_line_number(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Show line numbers.";
    const LONG: &str = long!(
        "\
Show line numbers (1-based). This is enabled by default when searching in a
terminal.
"
    );
    let arg = RGArg::switch("line-number")
        .short("n")
        .help(SHORT)
        .long_help(LONG)
        .overrides("no-line-number");
    args.push(arg);

    const NO_SHORT: &str = "Suppress line numbers.";
    const NO_LONG: &str = long!(
        "\
Suppress line numbers. This is enabled by default when not searching in a
terminal.
"
    );
    let arg = RGArg::switch("no-line-number")
        .short("N")
        .help(NO_SHORT)
        .long_help(NO_LONG)
        .overrides("line-number");
    args.push(arg);
}

fn flag_line_regexp(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Only show matches surrounded by line boundaries.";
    const LONG: &str = long!(
        "\
Only show matches surrounded by line boundaries. This is equivalent to putting
^...$ around all of the search patterns. In other words, this only prints lines
where the entire line participates in a match.

This overrides the --word-regexp flag.
"
    );
    let arg = RGArg::switch("line-regexp")
        .short("x")
        .help(SHORT)
        .long_help(LONG)
        .overrides("word-regexp");
    args.push(arg);
}

fn flag_max_columns(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Don't print lines longer than this limit.";
    const LONG: &str = long!(
        "\
Don't print lines longer than this limit in bytes. Longer lines are omitted,
and only the number of matches in that line is printed.

When this flag is omitted or is set to 0, then it has no effect.
"
    );
    let arg = RGArg::flag("max-columns", "NUM")
        .short("M")
        .help(SHORT)
        .long_help(LONG)
        .number();
    args.push(arg);
}

fn flag_max_columns_preview(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Print a preview for lines exceeding the limit.";
    const LONG: &str = long!(
        "\
When the '--max-columns' flag is used, ripgrep will by default completely
replace any line that is too long with a message indicating that a matching
line was removed. When this flag is combined with '--max-columns', a preview
of the line (corresponding to the limit size) is shown instead, where the part
of the line exceeding the limit is not shown.

If the '--max-columns' flag is not set, then this has no effect.

This flag can be disabled with '--no-max-columns-preview'.
"
    );
    let arg = RGArg::switch("max-columns-preview")
        .help(SHORT)
        .long_help(LONG)
        .overrides("no-max-columns-preview");
    args.push(arg);

    let arg = RGArg::switch("no-max-columns-preview")
        .hidden()
        .overrides("max-columns-preview");
    args.push(arg);
}

fn flag_max_count(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Limit the number of matches.";
    const LONG: &str = long!(
        "\
Limit the number of matching lines per file searched to NUM.
"
    );
    let arg = RGArg::flag("max-count", "NUM")
        .short("m")
        .help(SHORT)
        .long_help(LONG)
        .number();
    args.push(arg);
}

fn flag_max_depth(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Descend at most NUM directories.";
    const LONG: &str = long!(
        "\
Limit the depth of directory traversal to NUM levels beyond the paths given. A
value of zero only searches the explicitly given paths themselves.

For example, 'rg --max-depth 0 dir/' is a no-op because dir/ will not be
descended into. 'rg --max-depth 1 dir/' will search only the direct children of
'dir'.
"
    );
    let arg = RGArg::flag("max-depth", "NUM")
        .help(SHORT)
        .long_help(LONG)
        .alias("maxdepth")
        .number();
    args.push(arg);
}

fn flag_max_filesize(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Ignore files larger than NUM in size.";
    const LONG: &str = long!(
        "\
Ignore files larger than NUM in size. This does not apply to directories.

The input format accepts suffixes of K, M or G which correspond to kilobytes,
megabytes and gigabytes, respectively. If no suffix is provided the input is
treated as bytes.

Examples: --max-filesize 50K or --max-filesize 80M
"
    );
    let arg =
        RGArg::flag("max-filesize", "NUM+SUFFIX?").help(SHORT).long_help(LONG);
    args.push(arg);
}

fn flag_mmap(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Search using memory maps when possible.";
    const LONG: &str = long!(
        "\
Search using memory maps when possible. This is enabled by default when ripgrep
thinks it will be faster.

Memory map searching doesn't currently support all options, so if an
incompatible option (e.g., --context) is given with --mmap, then memory maps
will not be used.

Note that ripgrep may abort unexpectedly when --mmap if it searches a file that
is simultaneously truncated.

This flag overrides --no-mmap.
"
    );
    let arg =
        RGArg::switch("mmap").help(SHORT).long_help(LONG).overrides("no-mmap");
    args.push(arg);

    const NO_SHORT: &str = "Never use memory maps.";
    const NO_LONG: &str = long!(
        "\
Never use memory maps, even when they might be faster.

This flag overrides --mmap.
"
    );
    let arg = RGArg::switch("no-mmap")
        .help(NO_SHORT)
        .long_help(NO_LONG)
        .overrides("mmap");
    args.push(arg);
}

fn flag_multiline(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Enable matching across multiple lines.";
    const LONG: &str = long!(
        "\
Enable matching across multiple lines.

When multiline mode is enabled, ripgrep will lift the restriction that a match
cannot include a line terminator. For example, when multiline mode is not
enabled (the default), then the regex '\\p{any}' will match any Unicode
codepoint other than '\\n'. Similarly, the regex '\\n' is explicitly forbidden,
and if you try to use it, ripgrep will return an error. However, when multiline
mode is enabled, '\\p{any}' will match any Unicode codepoint, including '\\n',
and regexes like '\\n' are permitted.

An important caveat is that multiline mode does not change the match semantics
of '.'. Namely, in most regex matchers, a '.' will by default match any
character other than '\\n', and this is true in ripgrep as well. In order to
make '.' match '\\n', you must enable the \"dot all\" flag inside the regex.
For example, both '(?s).' and '(?s:.)' have the same semantics, where '.' will
match any character, including '\\n'. Alternatively, the '--multiline-dotall'
flag may be passed to make the \"dot all\" behavior the default. This flag only
applies when multiline search is enabled.

There is no limit on the number of the lines that a single match can span.

**WARNING**: Because of how the underlying regex engine works, multiline
searches may be slower than normal line-oriented searches, and they may also
use more memory. In particular, when multiline mode is enabled, ripgrep
requires that each file it searches is laid out contiguously in memory
(either by reading it onto the heap or by memory-mapping it). Things that
cannot be memory-mapped (such as stdin) will be consumed until EOF before
searching can begin. In general, ripgrep will only do these things when
necessary. Specifically, if the --multiline flag is provided but the regex
does not contain patterns that would match '\\n' characters, then ripgrep
will automatically avoid reading each file into memory before searching it.
Nevertheless, if you only care about matches spanning at most one line, then it
is always better to disable multiline mode.

This flag can be disabled with --no-multiline.
"
    );
    let arg = RGArg::switch("multiline")
        .short("U")
        .help(SHORT)
        .long_help(LONG)
        .overrides("no-multiline");
    args.push(arg);

    let arg = RGArg::switch("no-multiline").hidden().overrides("multiline");
    args.push(arg);
}

fn flag_multiline_dotall(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Make '.' match new lines when multiline is enabled.";
    const LONG: &str = long!(
        "\
This flag enables \"dot all\" in your regex pattern, which causes '.' to match
newlines when multiline searching is enabled. This flag has no effect if
multiline searching isn't enabled with the --multiline flag.

Normally, a '.' will match any character except newlines. While this behavior
typically isn't relevant for line-oriented matching (since matches can span at
most one line), this can be useful when searching with the -U/--multiline flag.
By default, the multiline mode runs without this flag.

This flag is generally intended to be used in an alias or your ripgrep config
file if you prefer \"dot all\" semantics by default. Note that regardless of
whether this flag is used, \"dot all\" semantics can still be controlled via
inline flags in the regex pattern itself, e.g., '(?s:.)' always enables \"dot
all\" whereas '(?-s:.)' always disables \"dot all\".

This flag can be disabled with --no-multiline-dotall.
"
    );
    let arg = RGArg::switch("multiline-dotall")
        .help(SHORT)
        .long_help(LONG)
        .overrides("no-multiline-dotall");
    args.push(arg);

    let arg = RGArg::switch("no-multiline-dotall")
        .hidden()
        .overrides("multiline-dotall");
    args.push(arg);
}

fn flag_no_config(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Never read configuration files.";
    const LONG: &str = long!(
        "\
Never read configuration files. When this flag is present, ripgrep will not
respect the RIPGREP_CONFIG_PATH environment variable.

If ripgrep ever grows a feature to automatically read configuration files in
pre-defined locations, then this flag will also disable that behavior as well.
"
    );
    let arg = RGArg::switch("no-config").help(SHORT).long_help(LONG);
    args.push(arg);
}

fn flag_no_ignore(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Don't respect ignore files.";
    const LONG: &str = long!(
        "\
Don't respect ignore files (.gitignore, .ignore, etc.). This implies
--no-ignore-dot, --no-ignore-exclude, --no-ignore-global, no-ignore-parent and
--no-ignore-vcs.

This does *not* imply --no-ignore-files, since --ignore-file is specified
explicitly as a command line argument.

When given only once, the -u flag is identical in behavior to --no-ignore and
can be considered an alias. However, subsequent -u flags have additional
effects; see --unrestricted.

This flag can be disabled with the --ignore flag.
"
    );
    let arg = RGArg::switch("no-ignore")
        .help(SHORT)
        .long_help(LONG)
        .overrides("ignore");
    args.push(arg);

    let arg = RGArg::switch("ignore").hidden().overrides("no-ignore");
    args.push(arg);
}

fn flag_no_ignore_dot(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Don't respect .ignore files.";
    const LONG: &str = long!(
        "\
Don't respect .ignore files.

This does *not* affect whether ripgrep will ignore files and directories
whose names begin with a dot. For that, see the -./--hidden flag.

This flag can be disabled with the --ignore-dot flag.
"
    );
    let arg = RGArg::switch("no-ignore-dot")
        .help(SHORT)
        .long_help(LONG)
        .overrides("ignore-dot");
    args.push(arg);

    let arg = RGArg::switch("ignore-dot").hidden().overrides("no-ignore-dot");
    args.push(arg);
}

fn flag_no_ignore_exclude(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Don't respect local exclusion files.";
    const LONG: &str = long!(
        "\
Don't respect ignore files that are manually configured for the repository
such as git's '.git/info/exclude'.

This flag can be disabled with the --ignore-exclude flag.
"
    );
    let arg = RGArg::switch("no-ignore-exclude")
        .help(SHORT)
        .long_help(LONG)
        .overrides("ignore-exclude");
    args.push(arg);

    let arg = RGArg::switch("ignore-exclude")
        .hidden()
        .overrides("no-ignore-exclude");
    args.push(arg);
}

fn flag_no_ignore_files(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Don't respect --ignore-file arguments.";
    const LONG: &str = long!(
        "\
When set, any --ignore-file flags, even ones that come after this flag, are
ignored.

This flag can be disabled with the --ignore-files flag.
"
    );
    let arg = RGArg::switch("no-ignore-files")
        .help(SHORT)
        .long_help(LONG)
        .overrides("ignore-files");
    args.push(arg);

    let arg =
        RGArg::switch("ignore-files").hidden().overrides("no-ignore-files");
    args.push(arg);
}

fn flag_no_ignore_global(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Don't respect global ignore files.";
    const LONG: &str = long!(
        "\
Don't respect ignore files that come from \"global\" sources such as git's
`core.excludesFile` configuration option (which defaults to
`$HOME/.config/git/ignore`).

This flag can be disabled with the --ignore-global flag.
"
    );
    let arg = RGArg::switch("no-ignore-global")
        .help(SHORT)
        .long_help(LONG)
        .overrides("ignore-global");
    args.push(arg);

    let arg =
        RGArg::switch("ignore-global").hidden().overrides("no-ignore-global");
    args.push(arg);
}

fn flag_no_ignore_messages(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Suppress gitignore parse error messages.";
    const LONG: &str = long!(
        "\
Suppresses all error messages related to parsing ignore files such as .ignore
or .gitignore.

This flag can be disabled with the --ignore-messages flag.
"
    );
    let arg = RGArg::switch("no-ignore-messages")
        .help(SHORT)
        .long_help(LONG)
        .overrides("ignore-messages");
    args.push(arg);

    let arg = RGArg::switch("ignore-messages")
        .hidden()
        .overrides("no-ignore-messages");
    args.push(arg);
}

fn flag_no_ignore_parent(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Don't respect ignore files in parent directories.";
    const LONG: &str = long!(
        "\
Don't respect ignore files (.gitignore, .ignore, etc.) in parent directories.

This flag can be disabled with the --ignore-parent flag.
"
    );
    let arg = RGArg::switch("no-ignore-parent")
        .help(SHORT)
        .long_help(LONG)
        .overrides("ignore-parent");
    args.push(arg);

    let arg =
        RGArg::switch("ignore-parent").hidden().overrides("no-ignore-parent");
    args.push(arg);
}

fn flag_no_ignore_vcs(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Don't respect VCS ignore files.";
    const LONG: &str = long!(
        "\
Don't respect version control ignore files (.gitignore, etc.). This implies
--no-ignore-parent for VCS files. Note that .ignore files will continue to be
respected.

This flag can be disabled with the --ignore-vcs flag.
"
    );
    let arg = RGArg::switch("no-ignore-vcs")
        .help(SHORT)
        .long_help(LONG)
        .overrides("ignore-vcs");
    args.push(arg);

    let arg = RGArg::switch("ignore-vcs").hidden().overrides("no-ignore-vcs");
    args.push(arg);
}

fn flag_no_messages(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Suppress some error messages.";
    const LONG: &str = long!(
        "\
Suppress all error messages related to opening and reading files. Error
messages related to the syntax of the pattern given are still shown.

This flag can be disabled with the --messages flag.
"
    );
    let arg = RGArg::switch("no-messages")
        .help(SHORT)
        .long_help(LONG)
        .overrides("messages");
    args.push(arg);

    let arg = RGArg::switch("messages").hidden().overrides("no-messages");
    args.push(arg);
}

fn flag_no_pcre2_unicode(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Disable Unicode mode for PCRE2 matching.";
    const LONG: &str = long!(
        "\
DEPRECATED. Use --no-unicode instead.

This flag is now an alias for --no-unicode. And --pcre2-unicode is an alias
for --unicode.
"
    );
    let arg = RGArg::switch("no-pcre2-unicode")
        .help(SHORT)
        .long_help(LONG)
        .overrides("pcre2-unicode")
        .overrides("unicode");
    args.push(arg);

    let arg = RGArg::switch("pcre2-unicode")
        .hidden()
        .overrides("no-pcre2-unicode")
        .overrides("no-unicode");
    args.push(arg);
}

fn flag_no_require_git(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Do not require a git repository to use gitignores.";
    const LONG: &str = long!(
        "\
By default, ripgrep will only respect global gitignore rules, .gitignore rules
and local exclude rules if ripgrep detects that you are searching inside a
git repository. This flag allows you to relax this restriction such that
ripgrep will respect all git related ignore rules regardless of whether you're
searching in a git repository or not.

This flag can be disabled with --require-git.
"
    );
    let arg = RGArg::switch("no-require-git")
        .help(SHORT)
        .long_help(LONG)
        .overrides("require-git");
    args.push(arg);

    let arg =
        RGArg::switch("require-git").hidden().overrides("no-require-git");
    args.push(arg);
}

fn flag_no_unicode(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Disable Unicode mode.";
    const LONG: &str = long!(
        "\
By default, ripgrep will enable \"Unicode mode\" in all of its regexes. This
has a number of consequences:

* '.' will only match valid UTF-8 encoded scalar values.
* Classes like '\\w', '\\s', '\\d' are all Unicode aware and much bigger
  than their ASCII only versions.
* Case insensitive matching will use Unicode case folding.
* A large array of classes like '\\p{Emoji}' are available.
* Word boundaries ('\\b' and '\\B') use the Unicode definition of a word
  character.

In some cases it can be desirable to turn these things off. The --no-unicode
flag will do exactly that.

For PCRE2 specifically, Unicode mode represents a critical trade off in the
user experience of ripgrep. In particular, unlike the default regex engine,
PCRE2 does not support the ability to search possibly invalid UTF-8 with
Unicode features enabled. Instead, PCRE2 *requires* that everything it searches
when Unicode mode is enabled is valid UTF-8. (Or valid UTF-16/UTF-32, but for
the purposes of ripgrep, we only discuss UTF-8.) This means that if you have
PCRE2's Unicode mode enabled and you attempt to search invalid UTF-8, then
the search for that file will halt and print an error. For this reason, when
PCRE2's Unicode mode is enabled, ripgrep will automatically \"fix\" invalid
UTF-8 sequences by replacing them with the Unicode replacement codepoint. This
penalty does not occur when using the default regex engine.

If you would rather see the encoding errors surfaced by PCRE2 when Unicode mode
is enabled, then pass the --no-encoding flag to disable all transcoding.

The --no-unicode flag can be disabled with --unicode. Note that
--no-pcre2-unicode and --pcre2-unicode are aliases for --no-unicode and
--unicode, respectively.
"
    );
    let arg = RGArg::switch("no-unicode")
        .help(SHORT)
        .long_help(LONG)
        .overrides("unicode")
        .overrides("pcre2-unicode");
    args.push(arg);

    let arg = RGArg::switch("unicode")
        .hidden()
        .overrides("no-unicode")
        .overrides("no-pcre2-unicode");
    args.push(arg);
}

fn flag_null(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Print a NUL byte after file paths.";
    const LONG: &str = long!(
        "\
Whenever a file path is printed, follow it with a NUL byte. This includes
printing file paths before matches, and when printing a list of matching files
such as with --count, --files-with-matches and --files. This option is useful
for use with xargs.
"
    );
    let arg = RGArg::switch("null").short("0").help(SHORT).long_help(LONG);
    args.push(arg);
}

fn flag_null_data(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Use NUL as a line terminator instead of \\n.";
    const LONG: &str = long!(
        "\
Enabling this option causes ripgrep to use NUL as a line terminator instead of
the default of '\\n'.

This is useful when searching large binary files that would otherwise have very
long lines if '\\n' were used as the line terminator. In particular, ripgrep
requires that, at a minimum, each line must fit into memory. Using NUL instead
can be a useful stopgap to keep memory requirements low and avoid OOM (out of
memory) conditions.

This is also useful for processing NUL delimited data, such as that emitted
when using ripgrep's -0/--null flag or find's --print0 flag.

Using this flag implies -a/--text.
"
    );
    let arg = RGArg::switch("null-data")
        .help(SHORT)
        .long_help(LONG)
        .overrides("crlf");
    args.push(arg);
}

fn flag_one_file_system(args: &mut Vec<RGArg>) {
    const SHORT: &str =
        "Do not descend into directories on other file systems.";
    const LONG: &str = long!(
        "\
When enabled, ripgrep will not cross file system boundaries relative to where
the search started from.

Note that this applies to each path argument given to ripgrep. For example, in
the command 'rg --one-file-system /foo/bar /quux/baz', ripgrep will search both
'/foo/bar' and '/quux/baz' even if they are on different file systems, but will
not cross a file system boundary when traversing each path's directory tree.

This is similar to find's '-xdev' or '-mount' flag.

This flag can be disabled with --no-one-file-system.
"
    );
    let arg = RGArg::switch("one-file-system")
        .help(SHORT)
        .long_help(LONG)
        .overrides("no-one-file-system");
    args.push(arg);

    let arg = RGArg::switch("no-one-file-system")
        .hidden()
        .overrides("one-file-system");
    args.push(arg);
}

fn flag_only_matching(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Print only matched parts of a line.";
    const LONG: &str = long!(
        "\
Print only the matched (non-empty) parts of a matching line, with each such
part on a separate output line.
"
    );
    let arg =
        RGArg::switch("only-matching").short("o").help(SHORT).long_help(LONG);
    args.push(arg);
}

fn flag_path_separator(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Set the path separator.";
    const LONG: &str = long!(
        "\
Set the path separator to use when printing file paths. This defaults to your
platform's path separator, which is / on Unix and \\ on Windows. This flag is
intended for overriding the default when the environment demands it (e.g.,
cygwin). A path separator is limited to a single byte.
"
    );
    let arg =
        RGArg::flag("path-separator", "SEPARATOR").help(SHORT).long_help(LONG);
    args.push(arg);
}

fn flag_passthru(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Print both matching and non-matching lines.";
    const LONG: &str = long!(
        "\
Print both matching and non-matching lines.

Another way to achieve a similar effect is by modifying your pattern to match
the empty string. For example, if you are searching using 'rg foo' then using
'rg \"^|foo\"' instead will emit every line in every file searched, but only
occurrences of 'foo' will be highlighted. This flag enables the same behavior
without needing to modify the pattern.

This overrides the --context, --after-context and --before-context flags.
"
    );
    let arg = RGArg::switch("passthru")
        .help(SHORT)
        .long_help(LONG)
        .alias("passthrough")
        .overrides("after-context")
        .overrides("before-context")
        .overrides("context");
    args.push(arg);
}

fn flag_pcre2(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Enable PCRE2 matching.";
    const LONG: &str = long!(
        "\
When this flag is present, ripgrep will use the PCRE2 regex engine instead of
its default regex engine.

This is generally useful when you want to use features such as look-around
or backreferences.

Note that PCRE2 is an optional ripgrep feature. If PCRE2 wasn't included in
your build of ripgrep, then using this flag will result in ripgrep printing
an error message and exiting. PCRE2 may also have worse user experience in
some cases, since it has fewer introspection APIs than ripgrep's default regex
engine. For example, if you use a '\\n' in a PCRE2 regex without the
'-U/--multiline' flag, then ripgrep will silently fail to match anything
instead of reporting an error immediately (like it does with the default
regex engine).

Related flags: --no-pcre2-unicode

This flag can be disabled with --no-pcre2.
"
    );
    let arg = RGArg::switch("pcre2")
        .short("P")
        .help(SHORT)
        .long_help(LONG)
        .overrides("no-pcre2")
        .overrides("auto-hybrid-regex")
        .overrides("no-auto-hybrid-regex")
        .overrides("engine");
    args.push(arg);

    let arg = RGArg::switch("no-pcre2")
        .hidden()
        .overrides("pcre2")
        .overrides("auto-hybrid-regex")
        .overrides("no-auto-hybrid-regex")
        .overrides("engine");
    args.push(arg);
}

fn flag_pcre2_version(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Print the version of PCRE2 that ripgrep uses.";
    const LONG: &str = long!(
        "\
When this flag is present, ripgrep will print the version of PCRE2 in use,
along with other information, and then exit. If PCRE2 is not available, then
ripgrep will print an error message and exit with an error code.
"
    );
    let arg = RGArg::switch("pcre2-version").help(SHORT).long_help(LONG);
    args.push(arg);
}

fn flag_pre(args: &mut Vec<RGArg>) {
    const SHORT: &str = "search outputs of COMMAND FILE for each FILE";
    const LONG: &str = long!(
        "\
For each input FILE, search the standard output of COMMAND FILE rather than the
contents of FILE. This option expects the COMMAND program to either be an
absolute path or to be available in your PATH. Either an empty string COMMAND
or the '--no-pre' flag will disable this behavior.

    WARNING: When this flag is set, ripgrep will unconditionally spawn a
    process for every file that is searched. Therefore, this can incur an
    unnecessarily large performance penalty if you don't otherwise need the
    flexibility offered by this flag. One possible mitigation to this is to use
    the '--pre-glob' flag to limit which files a preprocessor is run with.

A preprocessor is not run when ripgrep is searching stdin.

When searching over sets of files that may require one of several decoders
as preprocessors, COMMAND should be a wrapper program or script which first
classifies FILE based on magic numbers/content or based on the FILE name and
then dispatches to an appropriate preprocessor. Each COMMAND also has its
standard input connected to FILE for convenience.

For example, a shell script for COMMAND might look like:

    case \"$1\" in
    *.pdf)
        exec pdftotext \"$1\" -
        ;;
    *)
        case $(file \"$1\") in
        *Zstandard*)
            exec pzstd -cdq
            ;;
        *)
            exec cat
            ;;
        esac
        ;;
    esac

The above script uses `pdftotext` to convert a PDF file to plain text. For
all other files, the script uses the `file` utility to sniff the type of the
file based on its contents. If it is a compressed file in the Zstandard format,
then `pzstd` is used to decompress the contents to stdout.

This overrides the -z/--search-zip flag.
"
    );
    let arg = RGArg::flag("pre", "COMMAND")
        .help(SHORT)
        .long_help(LONG)
        .overrides("no-pre")
        .overrides("search-zip");
    args.push(arg);

    let arg = RGArg::switch("no-pre").hidden().overrides("pre");
    args.push(arg);
}

fn flag_pre_glob(args: &mut Vec<RGArg>) {
    const SHORT: &str =
        "Include or exclude files from a preprocessing command.";
    const LONG: &str = long!(
        "\
This flag works in conjunction with the --pre flag. Namely, when one or more
--pre-glob flags are given, then only files that match the given set of globs
will be handed to the command specified by the --pre flag. Any non-matching
files will be searched without using the preprocessor command.

This flag is useful when searching many files with the --pre flag. Namely,
it permits the ability to avoid process overhead for files that don't need
preprocessing. For example, given the following shell script, 'pre-pdftotext':

    #!/bin/sh

    pdftotext \"$1\" -

then it is possible to use '--pre pre-pdftotext --pre-glob \'*.pdf\'' to make
it so ripgrep only executes the 'pre-pdftotext' command on files with a '.pdf'
extension.

Multiple --pre-glob flags may be used. Globbing rules match .gitignore globs.
Precede a glob with a ! to exclude it.

This flag has no effect if the --pre flag is not used.
"
    );
    let arg = RGArg::flag("pre-glob", "GLOB")
        .help(SHORT)
        .long_help(LONG)
        .multiple()
        .allow_leading_hyphen();
    args.push(arg);
}

fn flag_pretty(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Alias for --color always --heading --line-number.";
    const LONG: &str = long!(
        "\
This is a convenience alias for '--color always --heading --line-number'. This
flag is useful when you still want pretty output even if you're piping ripgrep
to another program or file. For example: 'rg -p foo | less -R'.
"
    );
    let arg = RGArg::switch("pretty").short("p").help(SHORT).long_help(LONG);
    args.push(arg);
}

fn flag_quiet(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Do not print anything to stdout.";
    const LONG: &str = long!(
        "\
Do not print anything to stdout. If a match is found in a file, then ripgrep
will stop searching. This is useful when ripgrep is used only for its exit
code (which will be an error if no matches are found).

When --files is used, then ripgrep will stop finding files after finding the
first file that matches all ignore rules.
"
    );
    let arg = RGArg::switch("quiet").short("q").help(SHORT).long_help(LONG);
    args.push(arg);
}

fn flag_regex_size_limit(args: &mut Vec<RGArg>) {
    const SHORT: &str = "The upper size limit of the compiled regex.";
    const LONG: &str = long!(
        "\
The upper size limit of the compiled regex. The default limit is 10M.

The argument accepts the same size suffixes as allowed in the --max-filesize
flag.
"
    );
    let arg = RGArg::flag("regex-size-limit", "NUM+SUFFIX?")
        .help(SHORT)
        .long_help(LONG);
    args.push(arg);
}

fn flag_regexp(args: &mut Vec<RGArg>) {
    const SHORT: &str = "A pattern to search for.";
    const LONG: &str = long!(
        "\
A pattern to search for. This option can be provided multiple times, where
all patterns given are searched. Lines matching at least one of the provided
patterns are printed. This flag can also be used when searching for patterns
that start with a dash.

For example, to search for the literal '-foo', you can use this flag:

    rg -e -foo

You can also use the special '--' delimiter to indicate that no more flags
will be provided. Namely, the following is equivalent to the above:

    rg -- -foo
"
    );
    let arg = RGArg::flag("regexp", "PATTERN")
        .short("e")
        .help(SHORT)
        .long_help(LONG)
        .multiple()
        .allow_leading_hyphen();
    args.push(arg);
}

fn flag_replace(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Replace matches with the given text.";
    const LONG: &str = long!(
        "\
Replace every match with the text given when printing results. Neither this
flag nor any other ripgrep flag will modify your files.

Capture group indices (e.g., $5) and names (e.g., $foo) are supported in the
replacement string. Capture group indices are numbered based on the position of
the opening parenthesis of the group, where the leftmost such group is $1. The
special $0 group corresponds to the entire match.

In shells such as Bash and zsh, you should wrap the pattern in single quotes
instead of double quotes. Otherwise, capture group indices will be replaced by
expanded shell variables which will most likely be empty.

To write a literal '$', use '$$'.

Note that the replacement by default replaces each match, and NOT the entire
line. To replace the entire line, you should match the entire line.

This flag can be used with the -o/--only-matching flag.
"
    );
    let arg = RGArg::flag("replace", "REPLACEMENT_TEXT")
        .short("r")
        .help(SHORT)
        .long_help(LONG)
        .allow_leading_hyphen();
    args.push(arg);
}

fn flag_search_zip(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Search in compressed files.";
    const LONG: &str = long!(
        "\
Search in compressed files. Currently gzip, bzip2, xz, LZ4, LZMA, Brotli and
Zstd files are supported. This option expects the decompression binaries to be
available in your PATH.

This flag can be disabled with --no-search-zip.
"
    );
    let arg = RGArg::switch("search-zip")
        .short("z")
        .help(SHORT)
        .long_help(LONG)
        .overrides("no-search-zip")
        .overrides("pre");
    args.push(arg);

    let arg = RGArg::switch("no-search-zip").hidden().overrides("search-zip");
    args.push(arg);
}

fn flag_smart_case(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Smart case search.";
    const LONG: &str = long!(
        "\
Searches case insensitively if the pattern is all lowercase. Search case
sensitively otherwise.

A pattern is considered all lowercase if both of the following rules hold:

First, the pattern contains at least one literal character. For example, 'a\\w'
contains a literal ('a') but just '\\w' does not.

Second, of the literals in the pattern, none of them are considered to be
uppercase according to Unicode. For example, 'foo\\pL' has no uppercase
literals but 'Foo\\pL' does.

This overrides the -s/--case-sensitive and -i/--ignore-case flags.
"
    );
    let arg = RGArg::switch("smart-case")
        .short("S")
        .help(SHORT)
        .long_help(LONG)
        .overrides("case-sensitive")
        .overrides("ignore-case");
    args.push(arg);
}

fn flag_sort_files(args: &mut Vec<RGArg>) {
    const SHORT: &str = "DEPRECATED";
    const LONG: &str = long!(
        "\
DEPRECATED: Use --sort or --sortr instead.

Sort results by file path. Note that this currently disables all parallelism
and runs search in a single thread.

This flag can be disabled with --no-sort-files.
"
    );
    let arg = RGArg::switch("sort-files")
        .help(SHORT)
        .long_help(LONG)
        .hidden()
        .overrides("no-sort-files")
        .overrides("sort")
        .overrides("sortr");
    args.push(arg);

    let arg = RGArg::switch("no-sort-files")
        .hidden()
        .overrides("sort-files")
        .overrides("sort")
        .overrides("sortr");
    args.push(arg);
}

fn flag_sort(args: &mut Vec<RGArg>) {
    const SHORT: &str =
        "Sort results in ascending order. Implies --threads=1.";
    const LONG: &str = long!(
        "\
This flag enables sorting of results in ascending order. The possible values
for this flag are:

    none      (Default) Do not sort results. Fastest. Can be multi-threaded.
    path      Sort by file path. Always single-threaded.
    modified  Sort by the last modified time on a file. Always single-threaded.
    accessed  Sort by the last accessed time on a file. Always single-threaded.
    created   Sort by the creation time on a file. Always single-threaded.

If the chosen (manually or by-default) sorting criteria isn't available on your
system (for example, creation time is not available on ext4 file systems), then
ripgrep will attempt to detect this, print an error and exit without searching.

To sort results in reverse or descending order, use the --sortr flag. Also,
this flag overrides --sortr.

Note that sorting results currently always forces ripgrep to abandon
parallelism and run in a single thread.
"
    );
    let arg = RGArg::flag("sort", "SORTBY")
        .help(SHORT)
        .long_help(LONG)
        .possible_values(&["path", "modified", "accessed", "created", "none"])
        .overrides("sortr")
        .overrides("sort-files")
        .overrides("no-sort-files");
    args.push(arg);
}

fn flag_sortr(args: &mut Vec<RGArg>) {
    const SHORT: &str =
        "Sort results in descending order. Implies --threads=1.";
    const LONG: &str = long!(
        "\
This flag enables sorting of results in descending order. The possible values
for this flag are:

    none      (Default) Do not sort results. Fastest. Can be multi-threaded.
    path      Sort by file path. Always single-threaded.
    modified  Sort by the last modified time on a file. Always single-threaded.
    accessed  Sort by the last accessed time on a file. Always single-threaded.
    created   Sort by the creation time on a file. Always single-threaded.

If the chosen (manually or by-default) sorting criteria isn't available on your
system (for example, creation time is not available on ext4 file systems), then
ripgrep will attempt to detect this, print an error and exit without searching.

To sort results in ascending order, use the --sort flag. Also, this flag
overrides --sort.

Note that sorting results currently always forces ripgrep to abandon
parallelism and run in a single thread.
"
    );
    let arg = RGArg::flag("sortr", "SORTBY")
        .help(SHORT)
        .long_help(LONG)
        .possible_values(&["path", "modified", "accessed", "created", "none"])
        .overrides("sort")
        .overrides("sort-files")
        .overrides("no-sort-files");
    args.push(arg);
}

fn flag_stats(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Print statistics about this ripgrep search.";
    const LONG: &str = long!(
        "\
Print aggregate statistics about this ripgrep search. When this flag is
present, ripgrep will print the following stats to stdout at the end of the
search: number of matched lines, number of files with matches, number of files
searched, and the time taken for the entire search to complete.

This set of aggregate statistics may expand over time.

Note that this flag has no effect if --files, --files-with-matches or
--files-without-match is passed.

This flag can be disabled with --no-stats.
"
    );
    let arg = RGArg::switch("stats")
        .help(SHORT)
        .long_help(LONG)
        .overrides("no-stats");
    args.push(arg);

    let arg = RGArg::switch("no-stats").hidden().overrides("stats");
    args.push(arg);
}

fn flag_text(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Search binary files as if they were text.";
    const LONG: &str = long!(
        "\
Search binary files as if they were text. When this flag is present, ripgrep's
binary file detection is disabled. This means that when a binary file is
searched, its contents may be printed if there is a match. This may cause
escape codes to be printed that alter the behavior of your terminal.

When binary file detection is enabled it is imperfect. In general, it uses
a simple heuristic. If a NUL byte is seen during search, then the file is
considered binary and search stops (unless this flag is present).
Alternatively, if the '--binary' flag is used, then ripgrep will only quit
when it sees a NUL byte after it sees a match (or searches the entire file).

This flag can be disabled with '--no-text'. It overrides the '--binary' flag.
"
    );
    let arg = RGArg::switch("text")
        .short("a")
        .help(SHORT)
        .long_help(LONG)
        .overrides("no-text")
        .overrides("binary")
        .overrides("no-binary");
    args.push(arg);

    let arg = RGArg::switch("no-text")
        .hidden()
        .overrides("text")
        .overrides("binary")
        .overrides("no-binary");
    args.push(arg);
}

fn flag_threads(args: &mut Vec<RGArg>) {
    const SHORT: &str = "The approximate number of threads to use.";
    const LONG: &str = long!(
        "\
The approximate number of threads to use. A value of 0 (which is the default)
causes ripgrep to choose the thread count using heuristics.
"
    );
    let arg =
        RGArg::flag("threads", "NUM").short("j").help(SHORT).long_help(LONG);
    args.push(arg);
}

fn flag_trim(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Trim prefixed whitespace from matches.";
    const LONG: &str = long!(
        "\
When set, all ASCII whitespace at the beginning of each line printed will be
trimmed.

This flag can be disabled with --no-trim.
"
    );
    let arg =
        RGArg::switch("trim").help(SHORT).long_help(LONG).overrides("no-trim");
    args.push(arg);

    let arg = RGArg::switch("no-trim").hidden().overrides("trim");
    args.push(arg);
}

fn flag_type(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Only search files matching TYPE.";
    const LONG: &str = long!(
        "\
Only search files matching TYPE. Multiple type flags may be provided. Use the
--type-list flag to list all available types.

This flag supports the special value 'all', which will behave as if --type
was provided for every file type supported by ripgrep (including any custom
file types). The end result is that '--type all' causes ripgrep to search in
\"whitelist\" mode, where it will only search files it recognizes via its type
definitions.
"
    );
    let arg = RGArg::flag("type", "TYPE")
        .short("t")
        .help(SHORT)
        .long_help(LONG)
        .multiple();
    args.push(arg);
}

fn flag_type_add(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Add a new glob for a file type.";
    const LONG: &str = long!(
        "\
Add a new glob for a particular file type. Only one glob can be added at a
time. Multiple --type-add flags can be provided. Unless --type-clear is used,
globs are added to any existing globs defined inside of ripgrep.

Note that this MUST be passed to every invocation of ripgrep. Type settings are
NOT persisted. See CONFIGURATION FILES for a workaround.

Example:

    rg --type-add 'foo:*.foo' -tfoo PATTERN.

--type-add can also be used to include rules from other types with the special
include directive. The include directive permits specifying one or more other
type names (separated by a comma) that have been defined and its rules will
automatically be imported into the type specified. For example, to create a
type called src that matches C++, Python and Markdown files, one can use:

    --type-add 'src:include:cpp,py,md'

Additional glob rules can still be added to the src type by using the
--type-add flag again:

    --type-add 'src:include:cpp,py,md' --type-add 'src:*.foo'

Note that type names must consist only of Unicode letters or numbers.
Punctuation characters are not allowed.
"
    );
    let arg = RGArg::flag("type-add", "TYPE_SPEC")
        .help(SHORT)
        .long_help(LONG)
        .multiple();
    args.push(arg);
}

fn flag_type_clear(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Clear globs for a file type.";
    const LONG: &str = long!(
        "\
Clear the file type globs previously defined for TYPE. This only clears the
default type definitions that are found inside of ripgrep.

Note that this MUST be passed to every invocation of ripgrep. Type settings are
NOT persisted. See CONFIGURATION FILES for a workaround.
"
    );
    let arg = RGArg::flag("type-clear", "TYPE")
        .help(SHORT)
        .long_help(LONG)
        .multiple();
    args.push(arg);
}

fn flag_type_not(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Do not search files matching TYPE.";
    const LONG: &str = long!(
        "\
Do not search files matching TYPE. Multiple type-not flags may be provided. Use
the --type-list flag to list all available types.
"
    );
    let arg = RGArg::flag("type-not", "TYPE")
        .short("T")
        .help(SHORT)
        .long_help(LONG)
        .multiple();
    args.push(arg);
}

fn flag_type_list(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Show all supported file types.";
    const LONG: &str = long!(
        "\
Show all supported file types and their corresponding globs.
"
    );
    let arg = RGArg::switch("type-list")
        .help(SHORT)
        .long_help(LONG)
        // This also technically conflicts with PATTERN, but the first file
        // path will actually be in PATTERN.
        .conflicts(&["file", "files", "pattern", "regexp"]);
    args.push(arg);
}

fn flag_unrestricted(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Reduce the level of \"smart\" searching.";
    const LONG: &str = long!(
        "\
Reduce the level of \"smart\" searching. A single -u won't respect .gitignore
(etc.) files (--no-ignore). Two -u flags will additionally search hidden files
and directories (-./--hidden). Three -u flags will additionally search binary
files (--binary).

'rg -uuu' is roughly equivalent to 'grep -r'.
"
    );
    let arg = RGArg::switch("unrestricted")
        .short("u")
        .help(SHORT)
        .long_help(LONG)
        .multiple();
    args.push(arg);
}

fn flag_vimgrep(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Show results in vim compatible format.";
    const LONG: &str = long!(
        "\
Show results with every match on its own line, including line numbers and
column numbers. With this option, a line with more than one match will be
printed more than once.
"
    );
    let arg = RGArg::switch("vimgrep").help(SHORT).long_help(LONG);
    args.push(arg);
}

fn flag_with_filename(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Print the file path with the matched lines.";
    const LONG: &str = long!(
        "\
Display the file path for matches. This is the default when more than one
file is searched. If --heading is enabled (the default when printing to a
terminal), the file path will be shown above clusters of matches from each
file; otherwise, the file name will be shown as a prefix for each matched line.

This flag overrides --no-filename.
"
    );
    let arg = RGArg::switch("with-filename")
        .short("H")
        .help(SHORT)
        .long_help(LONG)
        .overrides("no-filename");
    args.push(arg);

    const NO_SHORT: &str = "Never print the file path with the matched lines.";
    const NO_LONG: &str = long!(
        "\
Never print the file path with the matched lines. This is the default when
ripgrep is explicitly instructed to search one file or stdin.

This flag overrides --with-filename.
"
    );
    let arg = RGArg::switch("no-filename")
        .short("I")
        .help(NO_SHORT)
        .long_help(NO_LONG)
        .overrides("with-filename");
    args.push(arg);
}

fn flag_word_regexp(args: &mut Vec<RGArg>) {
    const SHORT: &str = "Only show matches surrounded by word boundaries.";
    const LONG: &str = long!(
        "\
Only show matches surrounded by word boundaries. This is roughly equivalent to
putting \\b before and after all of the search patterns.

This overrides the --line-regexp flag.
"
    );
    let arg = RGArg::switch("word-regexp")
        .short("w")
        .help(SHORT)
        .long_help(LONG)
        .overrides("line-regexp");
    args.push(arg);
}

fn flag_stop_on_nonmatch(args: &mut Vec<RGArg>) {
    const SHORT: &str = "After a successful match in a file, stop reading the file once a non-matching line is found.";
    const LONG: &str = long!(
        "\
Enabling this option will cause ripgrep to stop reading a file once it encounters
a non-matching line after it has encountered a matching one. This is useful if it
is expected that all matches in a given file will be on sequential lines, for example
due to the files being sorted and the pattern being matched on being at the start
of the line.

This overrides the -U/--multiline flag.
"
    );
    let arg = RGArg::switch("stop-on-nonmatch")
        .help(SHORT)
        .long_help(LONG)
        .overrides("multiline");
    args.push(arg);
}

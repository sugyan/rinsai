//! The options the engine declares, and what it currently does with them.
//!
//! **A declared option is a promise.** An engine that advertises `Threads` and
//! runs one thread has lied to its operator, and that lie corrupts an SPRT run
//! rather than crashing — which is far worse. So only options that exist are
//! declared, and the two that are declared before they are honoured say so out
//! loud at `isready`.

use std::fmt;

/// What kind of control a GUI should show, and its default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OptionKind {
    Check { default: bool },
    Spin { default: i64, min: i64, max: i64 },
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct OptionSpec {
    pub(crate) name: &'static str,
    pub(crate) kind: OptionKind,
    /// `None` once the engine acts on the option. Until then it names the step
    /// that will, and that text is reported to the operator at `isready`.
    pub(crate) planned: Option<&'static str>,
}

/// `USI_Hash` and `USI_Ponder` are the protocol's reserved options: every GUI
/// and harness sends them whether or not an engine declares them, so they have
/// to be parsed regardless, and declaring them is what puts them in the GUI's
/// settings dialog where an operator expects to find them.
///
/// `Threads` arrives at E2 with Lazy SMP, `EvalFile` at E3 with the network,
/// `BookFile` at E4. An option we do not declare but that someone sends is
/// still accepted silently, so nothing breaks in the meantime.
pub(crate) const OPTIONS: &[OptionSpec] = &[
    OptionSpec {
        name: "USI_Hash",
        kind: OptionKind::Spin {
            default: 256,
            min: 1,
            max: 65_536,
        },
        planned: Some("E0 step 3, with the transposition table"),
    },
    OptionSpec {
        name: "USI_Ponder",
        kind: OptionKind::Check { default: false },
        planned: Some("E2, with ponder"),
    },
];

impl fmt::Display for OptionSpec {
    /// The `option name …` line of the `usi` response.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "option name {} type ", self.name)?;
        match self.kind {
            OptionKind::Check { default } => write!(f, "check default {default}"),
            OptionKind::Spin { default, min, max } => {
                write!(f, "spin default {default} min {min} max {max}")
            }
        }
    }
}

/// The values an operator has set.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Options {
    pub(crate) hash_mb: i64,
    pub(crate) ponder: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            hash_mb: 256,
            ponder: false,
        }
    }
}

/// Why a `setoption` was not applied. Never fatal — the loop logs it and
/// carries on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum OptionError {
    Unknown(String),
    MissingValue(&'static str),
    BadValue { name: &'static str, value: String },
    OutOfRange { name: &'static str, value: i64 },
}

impl fmt::Display for OptionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown(name) => write!(f, "unknown option `{name}`"),
            Self::MissingValue(name) => write!(f, "option `{name}` needs a value"),
            Self::BadValue { name, value } => {
                write!(f, "option `{name}` cannot take `{value}`")
            }
            Self::OutOfRange { name, value } => {
                write!(f, "option `{name}` is out of range at {value}")
            }
        }
    }
}

impl Options {
    pub(crate) fn set(&mut self, name: &str, value: Option<&str>) -> Result<(), OptionError> {
        let spec = OPTIONS
            .iter()
            .find(|spec| spec.name == name)
            .ok_or_else(|| OptionError::Unknown(name.to_owned()))?;
        let value = value.ok_or(OptionError::MissingValue(spec.name))?;

        match spec.kind {
            OptionKind::Check { .. } => {
                self.ponder = value.parse().map_err(|_| OptionError::BadValue {
                    name: spec.name,
                    value: value.to_owned(),
                })?;
            }
            OptionKind::Spin { min, max, .. } => {
                let parsed: i64 = value.parse().map_err(|_| OptionError::BadValue {
                    name: spec.name,
                    value: value.to_owned(),
                })?;
                if parsed < min || parsed > max {
                    return Err(OptionError::OutOfRange {
                        name: spec.name,
                        value: parsed,
                    });
                }
                self.hash_mb = parsed;
            }
        }
        Ok(())
    }

    /// Options an operator has changed that the engine does not act on yet.
    ///
    /// Reported at `isready` so "accepted but unused" is disclosed where it can
    /// actually be seen, rather than only in a document. It fires only when
    /// something was changed, and each entry deletes itself as its step lands.
    pub(crate) fn unhonoured_changes(&self) -> Vec<(&'static str, &'static str)> {
        OPTIONS
            .iter()
            .filter(|spec| self.differs_from_default(spec))
            .filter_map(|spec| spec.planned.map(|planned| (spec.name, planned)))
            .collect()
    }

    fn differs_from_default(&self, spec: &OptionSpec) -> bool {
        match spec.kind {
            OptionKind::Check { default } => self.ponder != default,
            OptionKind::Spin { default, .. } => self.hash_mb != default,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_declared_lines_are_well_formed() {
        let lines: Vec<String> = OPTIONS.iter().map(ToString::to_string).collect();
        assert_eq!(
            lines,
            vec![
                "option name USI_Hash type spin default 256 min 1 max 65536",
                "option name USI_Ponder type check default false",
            ]
        );
    }

    #[test]
    fn known_options_are_stored() {
        let mut options = Options::default();
        assert_eq!(options.set("USI_Hash", Some("512")), Ok(()));
        assert_eq!(options.hash_mb, 512);
        assert_eq!(options.set("USI_Ponder", Some("true")), Ok(()));
        assert!(options.ponder);
    }

    #[test]
    fn bad_input_is_reported_and_changes_nothing() {
        let mut options = Options::default();
        assert_eq!(
            options.set("Nonsense", Some("3")),
            Err(OptionError::Unknown("Nonsense".to_owned()))
        );
        assert_eq!(
            options.set("USI_Hash", None),
            Err(OptionError::MissingValue("USI_Hash"))
        );
        assert_eq!(
            options.set("USI_Hash", Some("lots")),
            Err(OptionError::BadValue {
                name: "USI_Hash",
                value: "lots".to_owned()
            })
        );
        assert_eq!(
            options.set("USI_Hash", Some("999999")),
            Err(OptionError::OutOfRange {
                name: "USI_Hash",
                value: 999_999
            })
        );
        assert_eq!(options.hash_mb, Options::default().hash_mb);
    }

    /// The warning must fire only when the operator actually changed
    /// something — otherwise every session starts with noise nobody reads.
    #[test]
    fn only_changed_unhonoured_options_are_reported() {
        let mut options = Options::default();
        assert!(options.unhonoured_changes().is_empty());

        assert!(options.set("USI_Hash", Some("512")).is_ok());
        assert_eq!(
            options.unhonoured_changes(),
            vec![("USI_Hash", "E0 step 3, with the transposition table")]
        );
    }
}

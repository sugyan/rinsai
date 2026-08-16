//! The options the engine declares, and what it currently does with them.
//!
//! **A declared option is a promise.** An engine that advertises `Threads` and
//! runs one thread has lied to its operator, and that lie corrupts an SPRT run
//! rather than crashing — which is far worse. So only options that exist are
//! declared, and any that is declared before it is honoured carries a `planned`
//! note and says so out loud at `isready`.

use std::fmt;
use std::time::Duration;

/// Where a declared option's value is stored.
///
/// ⚠️ It exists so that [`Options::set`] dispatches on **which option** rather
/// than on what kind of control it is. Dispatching on the kind works only while
/// there is at most one option per kind: the moment `Threads` joins `USI_Hash`
/// as a second spin, `setoption name Threads value 4` silently writes the hash
/// size. Adding an option means adding a variant here, which makes the `match`
/// below non-exhaustive and fails to compile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Slot {
    HashMb,
    Ponder,
    DeliveryMarginMs,
}

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
    pub(crate) slot: Slot,
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
///
/// The spin bounds are **conventional, not measured**, but they are honoured:
/// `min 1` is a commitment that the transposition table works at 1 MiB, and the
/// default is read from `rinsai_search` rather than written twice, so the size
/// advertised is the size allocated.
pub(crate) const OPTIONS: &[OptionSpec] = &[
    OptionSpec {
        name: "USI_Hash",
        kind: OptionKind::Spin {
            default: rinsai_search::DEFAULT_HASH_MB as i64,
            min: 1,
            max: 65_536,
        },
        slot: Slot::HashMb,
        planned: None,
    },
    OptionSpec {
        name: "DeliveryMargin",
        kind: OptionKind::Spin {
            default: rinsai_search::DEFAULT_DELIVERY_MARGIN_MS as i64,
            min: 0,
            max: 10_000,
        },
        slot: Slot::DeliveryMarginMs,
        planned: None,
    },
    OptionSpec {
        name: "USI_Ponder",
        kind: OptionKind::Check { default: false },
        slot: Slot::Ponder,
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
    pub(crate) delivery_margin_ms: i64,
    pub(crate) ponder: bool,
}

impl Default for Options {
    /// ⚠️ **Every field here must equal what [`OPTIONS`] advertises**, or a GUI
    /// that sends no `setoption` gets an engine configured differently from the
    /// one its dialog shows.
    fn default() -> Self {
        Self {
            hash_mb: rinsai_search::DEFAULT_HASH_MB as i64,
            delivery_margin_ms: rinsai_search::DEFAULT_DELIVERY_MARGIN_MS as i64,
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
    /// Applies a `setoption`, answering **which slot moved** so the caller can
    /// act on the ones that reach past this table. Nothing is written on an
    /// error.
    pub(crate) fn set(&mut self, name: &str, value: Option<&str>) -> Result<Slot, OptionError> {
        let spec = OPTIONS
            .iter()
            .find(|spec| spec.name == name)
            .ok_or_else(|| OptionError::Unknown(name.to_owned()))?;
        let value = value.ok_or(OptionError::MissingValue(spec.name))?;

        // Dispatch on the slot, never on the kind — see `Slot`.
        match spec.slot {
            Slot::HashMb => self.hash_mb = spin(spec, value)?,
            Slot::DeliveryMarginMs => self.delivery_margin_ms = spin(spec, value)?,
            Slot::Ponder => self.ponder = check(spec, value)?,
        }
        Ok(spec.slot)
    }

    /// The table size to allocate, in MiB. Total rather than fallible: [`spin`]
    /// has already bounded the value against the spec's `min`/`max`.
    pub(crate) fn hash_mb(&self) -> usize {
        usize::try_from(self.hash_mb).unwrap_or(rinsai_search::DEFAULT_HASH_MB)
    }

    /// What the search keeps back from the clock it is charged against. Total
    /// for the same reason [`Self::hash_mb`] is.
    pub(crate) fn delivery_margin(&self) -> Duration {
        Duration::from_millis(
            u64::try_from(self.delivery_margin_ms)
                .unwrap_or(rinsai_search::DEFAULT_DELIVERY_MARGIN_MS),
        )
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
        match (spec.slot, spec.kind) {
            (Slot::HashMb, OptionKind::Spin { default, .. }) => self.hash_mb != default,
            (Slot::DeliveryMarginMs, OptionKind::Spin { default, .. }) => {
                self.delivery_margin_ms != default
            }
            (Slot::Ponder, OptionKind::Check { default }) => self.ponder != default,
            // A slot declared with a control it cannot hold is a table error,
            // not operator input; treat it as unchanged rather than guessing.
            _ => false,
        }
    }
}

fn spin(spec: &OptionSpec, value: &str) -> Result<i64, OptionError> {
    let OptionKind::Spin { min, max, .. } = spec.kind else {
        return Err(OptionError::BadValue {
            name: spec.name,
            value: value.to_owned(),
        });
    };
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
    Ok(parsed)
}

fn check(spec: &OptionSpec, value: &str) -> Result<bool, OptionError> {
    // The kind guard matches `spin` above. Without it this function would
    // happily accept `true` for an option the table declares as a spin, and a
    // (slot, kind) pair that disagrees would go unnoticed until an operator
    // wondered why a value did nothing — which is the class of silent lie this
    // module opens by warning against.
    let OptionKind::Check { .. } = spec.kind else {
        return Err(OptionError::BadValue {
            name: spec.name,
            value: value.to_owned(),
        });
    };
    value.parse().map_err(|_| OptionError::BadValue {
        name: spec.name,
        value: value.to_owned(),
    })
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
                "option name DeliveryMargin type spin default 30 min 0 max 10000",
                "option name USI_Ponder type check default false",
            ]
        );
    }

    /// Three copies of one number — advertised, held before any `setoption`, and
    /// allocated by `NegamaxSearcher::new` — so all three read one const.
    ///
    /// Sabotage: give the spin above a literal default.
    #[test]
    fn the_advertised_default_is_what_gets_allocated() {
        let spec = OPTIONS
            .iter()
            .find(|spec| spec.name == "USI_Hash")
            .expect("USI_Hash is declared");
        let OptionKind::Spin { default, min, .. } = spec.kind else {
            panic!("USI_Hash is a spin");
        };
        assert_eq!(default, rinsai_search::DEFAULT_HASH_MB as i64);
        assert_eq!(Options::default().hash_mb, default);
        assert_eq!(Options::default().hash_mb(), rinsai_search::DEFAULT_HASH_MB);
        // `min 1` is a promise that the table works at one MiB, which
        // `rinsai-search`'s own tests are what actually exercise.
        assert_eq!(min, 1);

        let spec = OPTIONS
            .iter()
            .find(|spec| spec.name == "DeliveryMargin")
            .expect("DeliveryMargin is declared");
        let OptionKind::Spin { default, .. } = spec.kind else {
            panic!("DeliveryMargin is a spin");
        };
        assert_eq!(default, rinsai_search::DEFAULT_DELIVERY_MARGIN_MS as i64);
        assert_eq!(Options::default().delivery_margin_ms, default);
        assert_eq!(
            Options::default().delivery_margin(),
            Duration::from_millis(rinsai_search::DEFAULT_DELIVERY_MARGIN_MS)
        );
    }

    #[test]
    fn known_options_are_stored() {
        let mut options = Options::default();
        assert_eq!(options.set("USI_Hash", Some("512")), Ok(Slot::HashMb));
        assert_eq!(options.hash_mb, 512);
        assert_eq!(options.hash_mb(), 512);
        assert_eq!(options.set("USI_Ponder", Some("true")), Ok(Slot::Ponder));
        assert!(options.ponder);
    }

    /// Setting one option must leave every other alone.
    ///
    /// This is the test for the `Slot` indirection. Sabotage: dispatch on
    /// `spec.kind` instead of `spec.slot` and add a second spin option (which is
    /// what `Threads` will be at E2) — `setoption name Threads value 4` then
    /// writes into `hash_mb` and this goes red. Written as a loop over `OPTIONS`
    /// so it covers whatever the table holds later, not just today's two.
    #[test]
    fn setting_one_option_does_not_disturb_the_others() {
        for spec in OPTIONS {
            let mut options = Options::default();
            let value = match spec.kind {
                OptionKind::Spin { default, max, .. } => (default + 1).min(max).to_string(),
                OptionKind::Check { default } => (!default).to_string(),
            };
            options
                .set(spec.name, Some(&value))
                .unwrap_or_else(|e| panic!("{}: {e}", spec.name));

            // …and it must have changed the one that *was* set. This is the
            // half that catches a table whose `slot` and `kind` disagree: such
            // an entry either fails `set` above or falls into
            // `differs_from_default`'s catch-all and reports "unchanged"
            // forever, which would silently disable its `isready` disclosure.
            assert!(
                options.differs_from_default(spec),
                "setting {} to {value} left it reading as unchanged — check its (slot, kind) pair",
                spec.name
            );

            for other in OPTIONS.iter().filter(|o| o.slot != spec.slot) {
                assert!(
                    !options.differs_from_default(other),
                    "setting {} changed {}",
                    spec.name,
                    other.name
                );
            }
        }
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

        assert!(options.set("USI_Ponder", Some("true")).is_ok());
        assert_eq!(
            options.unhonoured_changes(),
            vec![("USI_Ponder", "E2, with ponder")]
        );
    }

    /// …and an option the engine *does* act on must stop disclosing itself, or
    /// the disclosure stops meaning anything.
    ///
    /// Sabotage: leave `planned` set on `USI_Hash`.
    #[test]
    fn an_honoured_option_no_longer_discloses_itself() {
        let mut options = Options::default();
        assert!(options.set("USI_Hash", Some("512")).is_ok());
        assert!(
            options.unhonoured_changes().is_empty(),
            "{:?}",
            options.unhonoured_changes()
        );
    }
}

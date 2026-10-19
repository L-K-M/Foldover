//! How a folder's filesystem compares names (PLAN.md §4.10). Names that
//! differ in bytes can still name one file: `Report.txt` and `report.txt`
//! on a case-insensitive volume, or `café` with a composed or decomposed
//! `é` on a macOS volume.

/// Whether names differing only in letter case name one file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaseMatching {
    Exact,
    Insensitive,
}

/// Whether names differing only in Unicode normalization name one file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnicodeMatching {
    Exact,
    Normalized,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NameMatching {
    case: CaseMatching,
    unicode: UnicodeMatching,
}

impl NameMatching {
    /// Names match byte for byte, as on most Linux filesystems.
    pub const EXACT: Self = Self::new(CaseMatching::Exact, UnicodeMatching::Exact);

    pub const fn new(case: CaseMatching, unicode: UnicodeMatching) -> Self {
        Self { case, unicode }
    }

    pub const fn case(self) -> CaseMatching {
        self.case
    }

    pub const fn unicode(self) -> UnicodeMatching {
        self.unicode
    }

    /// The matching a pair must respect: two names collide when either
    /// folder would store them as one file.
    pub fn either(self, other: Self) -> Self {
        let case =
            if self.case == CaseMatching::Insensitive || other.case == CaseMatching::Insensitive {
                CaseMatching::Insensitive
            } else {
                CaseMatching::Exact
            };
        let unicode = if self.unicode == UnicodeMatching::Normalized
            || other.unicode == UnicodeMatching::Normalized
        {
            UnicodeMatching::Normalized
        } else {
            UnicodeMatching::Exact
        };
        Self { case, unicode }
    }
}

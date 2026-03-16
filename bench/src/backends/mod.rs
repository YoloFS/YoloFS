pub mod agfs;
pub mod branchfs;
pub mod native;
pub mod overlayfs;
pub mod try_backend;

use crate::backend::Backend;

/// All backends, in display order. Unavailable backends are included but
/// marked; callers should check `available()` before running.
pub fn all() -> Vec<Box<dyn Backend>> {
    vec![
        Box::new(native::Native),
        Box::new(agfs::AgfsAllowAll),
        Box::new(agfs::AgfsRealistic),
        Box::new(try_backend::Try),
        Box::new(overlayfs::Overlayfs),
        Box::new(branchfs::BranchFs),
    ]
}

pub fn by_name(name: &str) -> Option<Box<dyn Backend>> {
    all().into_iter().find(|b| b.name() == name)
}

/// Canonical display order for backend names (used by the report renderer).
pub fn display_order() -> Vec<&'static str> {
    all().iter().map(|b| b.name()).collect()
}

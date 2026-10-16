//! `foldover-fs` — driver layer. Local file I/O relative to an open root
//! handle: enumeration with attributes, atomic writes, clones, links and
//! file metadata. Calls the OS directly; depends on no other Foldover
//! crate. See PLAN.md §4.4.

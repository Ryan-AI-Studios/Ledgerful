fn rust_target() {}
pub fn rust_caller() { rust_target(); }
mod rust_defs;
use rust_defs::real_rust as rust_alias;
pub fn alias_rust_caller() { rust_alias(); }

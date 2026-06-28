use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

pub fn stable_hash_hex<T: Hash>(value: &T) -> String {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

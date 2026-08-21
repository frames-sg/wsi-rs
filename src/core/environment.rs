#[cfg(any(feature = "metal", feature = "cuda"))]
pub(crate) fn flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| flag_value(&value))
}

pub(crate) fn positive_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| positive_u64_value(&value))
        .unwrap_or(default)
}

#[cfg(any(test, feature = "metal", feature = "cuda"))]
fn flag_value(value: &str) -> bool {
    ["1", "true", "yes", "on"]
        .iter()
        .any(|enabled| value.eq_ignore_ascii_case(enabled))
}

fn positive_u64_value(value: &str) -> Option<u64> {
    value.parse::<u64>().ok().filter(|value| *value > 0)
}

#[cfg(test)]
mod tests;

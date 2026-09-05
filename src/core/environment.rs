pub(crate) fn positive_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| positive_u64_value(&value))
        .unwrap_or(default)
}

fn positive_u64_value(value: &str) -> Option<u64> {
    value.parse::<u64>().ok().filter(|value| *value > 0)
}

#[cfg(test)]
mod tests;

use crate::error::WsiError;

pub(crate) fn expect_exact_count<T>(
    values: Vec<T>,
    expected: usize,
    context: &'static str,
) -> Result<Vec<T>, WsiError> {
    if values.len() != expected {
        return Err(WsiError::BackendContract {
            context,
            expected,
            actual: values.len(),
        });
    }
    Ok(values)
}

pub(crate) fn exactly_one<T>(values: Vec<T>, context: &'static str) -> Result<T, WsiError> {
    let mut values = expect_exact_count(values, 1, context)?;
    Ok(values.pop().expect("length checked above"))
}

#[cfg(test)]
mod tests;

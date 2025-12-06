/// Small host-friendly helper used by unit tests.
pub fn add_numbers(a: i32, b: i32) -> i32 {
    a + b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_two_numbers() {
        assert_eq!(add_numbers(2, 2), 4);
    }
}

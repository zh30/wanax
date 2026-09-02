#!/bin/sh
set -eu
cat > src/lib.rs << 'EOF'
pub fn add(a: i32, b: i32) -> i32 { a + b }

#[cfg(test)]
mod tests {
    #[test]
    fn test_add() { assert_eq!(super::add(2, 3), 5); }
}
EOF

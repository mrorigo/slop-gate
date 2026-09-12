mod utility {
    impl Parser {
        fn parse_tokens(input: &str) -> usize {
            if input.is_empty() {
                return 0;
            }

            input
                .split_whitespace()
                .filter(|token| !token.is_empty())
                .count()
        }
    }
}

use noirc_errors::Span;

use crate::ast::{BlockExpression, Expression, ExpressionKind};

use super::Parser;

impl<'a> Parser<'a> {
    pub(crate) fn parse_expression(&mut self) -> Expression {
        // TODO: parse other expressions

        let start_span = self.current_token_span;

        let kind = if let Some(int) = self.eat_int() {
            ExpressionKind::integer(int)
        } else {
            return Expression { kind: ExpressionKind::Error, span: Span::default() };
        };

        Expression { kind, span: self.span_since(start_span) }
    }

    pub(super) fn parse_block_expression(&mut self) -> BlockExpression {
        self.eat_left_brace();
        // TODO: parse statements
        self.eat_right_brace();

        BlockExpression { statements: Vec::new() }
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        ast::{ExpressionKind, Literal},
        parser::Parser,
    };

    #[test]
    fn parses_integer_literal() {
        let src = "42";
        let expr = Parser::for_str(src).parse_expression();
        let ExpressionKind::Literal(Literal::Integer(field, negative)) = expr.kind else {
            panic!("Expected integer literal");
        };
        assert_eq!(field, 42_u128.into());
        assert!(!negative);
    }
}

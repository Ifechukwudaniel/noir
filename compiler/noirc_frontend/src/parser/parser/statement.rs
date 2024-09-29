use noirc_errors::{Span, Spanned};

use crate::{
    ast::{
        AssignStatement, BinaryOp, BinaryOpKind, ConstrainKind, ConstrainStatement, Expression,
        ExpressionKind, ForLoopStatement, ForRange, Ident, InfixExpression, LValue, LetStatement,
        Statement, StatementKind,
    },
    parser::ParserErrorReason,
    token::{Attribute, Keyword, Token, TokenKind},
};

use super::Parser;

impl<'a> Parser<'a> {
    pub(crate) fn parse_statement_or_error(&mut self) -> Statement {
        if let Some(statement) = self.parse_statement() {
            statement
        } else {
            self.push_error(
                ParserErrorReason::ExpectedStatementAfterThis,
                self.previous_token_span,
            );
            Statement { kind: StatementKind::Error, span: self.span_at_previous_token_end() }
        }
    }

    pub(crate) fn parse_statement(&mut self) -> Option<Statement> {
        let attributes = self.parse_attributes();

        let start_span = self.current_token_span;
        let kind = self.parse_statement_kind(attributes)?;
        let span = self.span_since(start_span);
        Some(Statement { kind, span })
    }

    fn parse_statement_kind(
        &mut self,
        attributes: Vec<(Attribute, Span)>,
    ) -> Option<StatementKind> {
        let start_span = self.current_token_span;

        if let Some(token) = self.eat_kind(TokenKind::InternedStatement) {
            match token.into_token() {
                Token::InternedStatement(statement) => {
                    return Some(StatementKind::Interned(statement))
                }
                _ => unreachable!(),
            }
        }

        if self.eat_keyword(Keyword::Break) {
            return Some(StatementKind::Break);
        }

        if self.eat_keyword(Keyword::Continue) {
            return Some(StatementKind::Continue);
        }

        if self.token.token() == &Token::Keyword(Keyword::Let) {
            let let_statement = self.parse_let_statement(attributes)?;
            return Some(StatementKind::Let(let_statement));
        }

        if let Some(constrain) = self.parse_constrain_statement() {
            return Some(StatementKind::Constrain(constrain));
        }

        if self.token.token() == &Token::Keyword(Keyword::Comptime) {
            return self.parse_comptime_statement(attributes);
        }

        if let Some(for_loop) = self.parse_for() {
            return Some(StatementKind::For(for_loop));
        }

        if let Some(kind) = self.parse_if_expr() {
            return Some(StatementKind::Expression(Expression {
                kind,
                span: self.span_since(start_span),
            }));
        }

        if let Some(block) = self.parse_block_expression() {
            return Some(StatementKind::Expression(Expression {
                kind: ExpressionKind::Block(block),
                span: self.span_since(start_span),
            }));
        }

        let expression = self.parse_expression()?;

        if self.eat_assign() {
            if let Some(lvalue) = LValue::from_expression(expression.clone()) {
                let expression = self.parse_expression_or_error();
                return Some(StatementKind::Assign(AssignStatement { lvalue, expression }));
            } else {
                // TODO: error (invalid l-value)
            }
        }

        if let Some(operator) = self.next_is_op_assign() {
            if let Some(lvalue) = LValue::from_expression(expression.clone()) {
                // Desugar `a <op>= b` to `a = a <op> b`. This relies on the evaluation of `a` having no side effects,
                // which is currently enforced by the restricted syntax of LValues.
                let infix = InfixExpression {
                    lhs: expression,
                    operator,
                    rhs: self.parse_expression_or_error(),
                };
                let expression = Expression::new(
                    ExpressionKind::Infix(Box::new(infix)),
                    self.span_since(start_span),
                );
                return Some(StatementKind::Assign(AssignStatement { lvalue, expression }));
            } else {
                // TODO: error (invalid l-value)
            }
        }

        Some(StatementKind::Expression(expression))
    }

    fn next_is_op_assign(&mut self) -> Option<BinaryOp> {
        let start_span = self.current_token_span;
        let operator = if self.next_token.token() == &Token::Assign {
            match self.token.token() {
                Token::Plus => Some(BinaryOpKind::Add),
                Token::Minus => Some(BinaryOpKind::Subtract),
                Token::Star => Some(BinaryOpKind::Multiply),
                Token::Slash => Some(BinaryOpKind::Divide),
                Token::Percent => Some(BinaryOpKind::Modulo),
                Token::Ampersand => Some(BinaryOpKind::And),
                Token::Caret => Some(BinaryOpKind::Xor),
                Token::ShiftLeft => Some(BinaryOpKind::ShiftLeft),
                Token::Pipe => Some(BinaryOpKind::Or),
                _ => None,
            }
        } else if self.token.token() == &Token::Greater
            && self.next_token.token() == &Token::GreaterEqual
        {
            Some(BinaryOpKind::ShiftRight)
        } else {
            None
        };

        if let Some(operator) = operator {
            self.next_token();
            self.next_token();
            Some(Spanned::from(self.span_since(start_span), operator))
        } else {
            None
        }
    }

    fn parse_for(&mut self) -> Option<ForLoopStatement> {
        let start_span = self.current_token_span;

        if !self.eat_keyword(Keyword::For) {
            return None;
        }

        let Some(identifier) = self.eat_ident() else {
            // TODO: error (expected for identifier)
            let identifier = Ident::default();
            return Some(self.empty_for_loop(identifier, start_span));
        };

        if !self.eat_keyword(Keyword::In) {
            // TODO: error (expected `in` after for identifier)
            return Some(self.empty_for_loop(identifier, start_span));
        }

        let expr = self.parse_expression_no_constructors_or_error();

        let range = if self.eat(Token::DoubleDot) {
            ForRange::Range(expr, self.parse_expression_no_constructors_or_error())
        } else {
            ForRange::Array(expr)
        };

        let block_start_span = self.current_token_span;
        let block = if let Some(block) = self.parse_block_expression() {
            Expression {
                kind: ExpressionKind::Block(block),
                span: self.span_since(block_start_span),
            }
        } else {
            // TODO: error (expected for body)
            Expression { kind: ExpressionKind::Error, span: self.span_since(block_start_span) }
        };

        Some(ForLoopStatement { identifier, range, block, span: self.span_since(start_span) })
    }

    fn empty_for_loop(&mut self, identifier: Ident, start_span: Span) -> ForLoopStatement {
        ForLoopStatement {
            identifier,
            range: ForRange::Array(Expression {
                kind: ExpressionKind::Error,
                span: Span::default(),
            }),
            block: Expression { kind: ExpressionKind::Error, span: Span::default() },
            span: self.span_since(start_span),
        }
    }

    fn parse_comptime_statement(
        &mut self,
        attributes: Vec<(Attribute, Span)>,
    ) -> Option<StatementKind> {
        if !self.eat_keyword(Keyword::Comptime) {
            return None;
        }

        let start_span = self.current_token_span;

        if let Some(block) = self.parse_block_expression() {
            let span = self.span_since(start_span);
            return Some(StatementKind::Comptime(Box::new(Statement {
                kind: StatementKind::Expression(Expression::new(
                    ExpressionKind::Block(block),
                    span,
                )),
                span,
            })));
        }

        if let Some(let_statement) = self.parse_let_statement(attributes) {
            return Some(StatementKind::Comptime(Box::new(Statement {
                kind: StatementKind::Let(let_statement),
                span: self.span_since(start_span),
            })));
        }

        if let Some(for_loop) = self.parse_for() {
            return Some(StatementKind::Comptime(Box::new(Statement {
                kind: StatementKind::For(for_loop),
                span: self.span_since(start_span),
            })));
        }

        // TODO: error (found comptime but not a valid statement)

        None
    }

    fn parse_let_statement(&mut self, attributes: Vec<(Attribute, Span)>) -> Option<LetStatement> {
        if !self.eat_keyword(Keyword::Let) {
            return None;
        }

        let attributes = self.validate_secondary_attributes(attributes);
        let pattern = self.parse_pattern();
        let r#type = self.parse_optional_type_annotation();
        let expression = if self.eat_assign() {
            self.parse_expression_or_error()
        } else {
            // TODO: error
            Expression { kind: ExpressionKind::Error, span: self.current_token_span }
        };

        Some(LetStatement { pattern, r#type, expression, attributes, comptime: false })
    }

    fn parse_constrain_statement(&mut self) -> Option<ConstrainStatement> {
        let start_span = self.current_token_span;
        let Some(kind) = self.parse_constrain_kind() else {
            return None;
        };

        Some(match kind {
            ConstrainKind::Assert | ConstrainKind::AssertEq => {
                let arguments = self.parse_arguments();
                if arguments.is_none() {
                    // TODO: error (expected arguments to assert/assert_eq)
                }
                let arguments = arguments.unwrap_or_default();

                ConstrainStatement { kind, arguments, span: self.span_since(start_span) }
            }
            ConstrainKind::Constrain => {
                self.push_error(ParserErrorReason::ConstrainDeprecated, self.previous_token_span);

                let expression = self.parse_expression_or_error();
                ConstrainStatement {
                    kind,
                    arguments: vec![expression],
                    span: self.span_since(start_span),
                }
            }
        })
    }

    fn parse_constrain_kind(&mut self) -> Option<ConstrainKind> {
        if self.eat_keyword(Keyword::Assert) {
            Some(ConstrainKind::Assert)
        } else if self.eat_keyword(Keyword::AssertEq) {
            Some(ConstrainKind::AssertEq)
        } else if self.eat_keyword(Keyword::Constrain) {
            Some(ConstrainKind::Constrain)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        ast::{ConstrainKind, ExpressionKind, ForRange, LValue, StatementKind, UnresolvedTypeData},
        parser::{
            parser::tests::{get_single_error, get_source_with_error_span},
            Parser, ParserErrorReason,
        },
    };

    #[test]
    fn parses_break() {
        let src = "break";
        let mut parser = Parser::for_str(src);
        let statement = parser.parse_statement_or_error();
        assert!(parser.errors.is_empty());
        assert!(matches!(statement.kind, StatementKind::Break));
    }

    #[test]
    fn parses_continue() {
        let src = "continue";
        let mut parser = Parser::for_str(src);
        let statement = parser.parse_statement_or_error();
        assert!(parser.errors.is_empty());
        assert!(matches!(statement.kind, StatementKind::Continue));
    }

    #[test]
    fn parses_let_statement_no_type() {
        let src = "let x = 1;";
        let mut parser = Parser::for_str(src);
        let statement = parser.parse_statement_or_error();
        assert!(parser.errors.is_empty());
        let StatementKind::Let(let_statement) = statement.kind else {
            panic!("Expected let statement");
        };
        assert_eq!(let_statement.pattern.to_string(), "x");
        assert!(matches!(let_statement.r#type.typ, UnresolvedTypeData::Unspecified));
        assert_eq!(let_statement.expression.to_string(), "1");
        assert!(!let_statement.comptime);
    }

    #[test]
    fn parses_let_statement_with_type() {
        let src = "let x: Field = 1;";
        let mut parser = Parser::for_str(src);
        let statement = parser.parse_statement_or_error();
        assert!(parser.errors.is_empty());
        let StatementKind::Let(let_statement) = statement.kind else {
            panic!("Expected let statement");
        };
        assert_eq!(let_statement.pattern.to_string(), "x");
        assert_eq!(let_statement.r#type.to_string(), "Field");
        assert_eq!(let_statement.expression.to_string(), "1");
        assert!(!let_statement.comptime);
    }

    #[test]
    fn parses_assert() {
        let src = "assert(true, \"good\")";
        let mut parser = Parser::for_str(src);
        let statement = parser.parse_statement_or_error();
        assert!(parser.errors.is_empty());
        let StatementKind::Constrain(constrain) = statement.kind else {
            panic!("Expected constrain statement");
        };
        assert_eq!(constrain.kind, ConstrainKind::Assert);
        assert_eq!(constrain.arguments.len(), 2);
    }

    #[test]
    fn parses_assert_eq() {
        let src = "assert_eq(1, 2, \"bad\")";
        let mut parser = Parser::for_str(src);
        let statement = parser.parse_statement_or_error();
        assert!(parser.errors.is_empty());
        let StatementKind::Constrain(constrain) = statement.kind else {
            panic!("Expected constrain statement");
        };
        assert_eq!(constrain.kind, ConstrainKind::AssertEq);
        assert_eq!(constrain.arguments.len(), 3);
    }

    #[test]
    fn parses_constrain() {
        let src = "
        constrain 1
        ^^^^^^^^^
        ";
        let (src, span) = get_source_with_error_span(src);
        let mut parser = Parser::for_str(&src);
        let statement = parser.parse_statement_or_error();
        let StatementKind::Constrain(constrain) = statement.kind else {
            panic!("Expected constrain statement");
        };
        assert_eq!(constrain.kind, ConstrainKind::Constrain);
        assert_eq!(constrain.arguments.len(), 1);

        let reason = get_single_error(&parser.errors, span);
        assert!(matches!(reason, ParserErrorReason::ConstrainDeprecated));
    }

    #[test]
    fn parses_comptime_block() {
        let src = "comptime { 1 }";
        let mut parser = Parser::for_str(&src);
        let statement = parser.parse_statement_or_error();
        assert!(parser.errors.is_empty());
        let StatementKind::Comptime(statement) = statement.kind else {
            panic!("Expected comptime statement");
        };
        let StatementKind::Expression(expr) = statement.kind else {
            panic!("Expected expression statement");
        };
        let ExpressionKind::Block(block) = expr.kind else {
            panic!("Expected block expression");
        };
        assert_eq!(block.statements.len(), 1);
    }

    #[test]
    fn parses_comptime_let() {
        let src = "comptime let x = 1;";
        let mut parser = Parser::for_str(&src);
        let statement = parser.parse_statement_or_error();
        assert!(parser.errors.is_empty());
        let StatementKind::Comptime(statement) = statement.kind else {
            panic!("Expected comptime statement");
        };
        let StatementKind::Let(..) = statement.kind else {
            panic!("Expected let statement");
        };
    }

    #[test]
    fn parses_for_array() {
        let src = "for i in x { }";
        let mut parser = Parser::for_str(&src);
        let statement = parser.parse_statement_or_error();
        assert!(parser.errors.is_empty());
        let StatementKind::For(for_loop) = statement.kind else {
            panic!("Expected for loop");
        };
        assert_eq!(for_loop.identifier.to_string(), "i");
        assert!(matches!(for_loop.range, ForRange::Array(..)));
    }

    #[test]
    fn parses_for_range() {
        let src = "for i in 0..10 { }";
        let mut parser = Parser::for_str(&src);
        let statement = parser.parse_statement_or_error();
        assert!(parser.errors.is_empty());
        let StatementKind::For(for_loop) = statement.kind else {
            panic!("Expected for loop");
        };
        assert_eq!(for_loop.identifier.to_string(), "i");
        assert!(matches!(for_loop.range, ForRange::Range(..)));
    }

    #[test]
    fn parses_comptime_for() {
        let src = "comptime for i in x { }";
        let mut parser = Parser::for_str(&src);
        let statement = parser.parse_statement_or_error();
        assert!(parser.errors.is_empty());
        let StatementKind::Comptime(statement) = statement.kind else {
            panic!("Expected comptime");
        };
        let StatementKind::For(for_loop) = statement.kind else {
            panic!("Expected for loop");
        };
        assert_eq!(for_loop.identifier.to_string(), "i");
        assert!(matches!(for_loop.range, ForRange::Array(..)));
    }

    #[test]
    fn parses_assignment() {
        let src = "x = 1";
        let mut parser = Parser::for_str(&src);
        let statement = parser.parse_statement_or_error();
        assert!(parser.errors.is_empty());
        let StatementKind::Assign(assign) = statement.kind else {
            panic!("Expected assign");
        };
        let LValue::Ident(ident) = assign.lvalue else {
            panic!("Expected ident");
        };
        assert_eq!(ident.to_string(), "x");
        assert_eq!(assign.expression.to_string(), "1");
    }

    #[test]
    fn parses_op_assignment() {
        let src = "x += 1";
        let mut parser = Parser::for_str(&src);
        let statement = parser.parse_statement_or_error();
        assert!(parser.errors.is_empty());
        let StatementKind::Assign(assign) = statement.kind else {
            panic!("Expected assign");
        };
        assert_eq!(assign.to_string(), "x = (x + 1)");
    }

    #[test]
    fn parses_op_assignment_with_shift_right() {
        let src = "x >>= 1";
        let mut parser = Parser::for_str(&src);
        let statement = parser.parse_statement_or_error();
        assert!(parser.errors.is_empty());
        let StatementKind::Assign(assign) = statement.kind else {
            panic!("Expected assign");
        };
        assert_eq!(assign.to_string(), "x = (x >> 1)");
    }

    #[test]
    fn parses_if_statement_followed_by_tuple() {
        // This shouldn't be parsed as a call
        let src = "{ if 1 { 2 } (3, 4) }";
        let mut parser = Parser::for_str(&src);
        let statement = parser.parse_statement_or_error();
        assert!(parser.errors.is_empty());
        let StatementKind::Expression(expr) = statement.kind else {
            panic!("Expected expr");
        };
        let ExpressionKind::Block(block) = expr.kind else {
            panic!("Expected block");
        };
        assert_eq!(block.statements.len(), 2);
    }

    #[test]
    fn parses_block_followed_by_tuple() {
        // This shouldn't be parsed as a call
        let src = "{ { 2 } (3, 4) }";
        let mut parser = Parser::for_str(&src);
        let statement = parser.parse_statement_or_error();
        assert!(parser.errors.is_empty());
        let StatementKind::Expression(expr) = statement.kind else {
            panic!("Expected expr");
        };
        let ExpressionKind::Block(block) = expr.kind else {
            panic!("Expected block");
        };
        assert_eq!(block.statements.len(), 2);
    }
}

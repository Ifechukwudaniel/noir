use std::iter::zip;

use noirc_errors::Span;

use noirc_frontend::ast::{ConstrainKind, ConstrainStatement, ForRange, Statement, StatementKind};

use crate::{rewrite, visitor::expr::wrap_exprs};

use super::{expr::NewlineMode, ExpressionType};

impl super::FmtVisitor<'_> {
    pub(crate) fn visit_stmts(&mut self, stmts: Vec<Statement>) {
        let len = stmts.len();

        for (Statement { kind, span }, index) in zip(stmts, 1..) {
            let is_last = index == len;
            self.visit_stmt(kind, span, is_last);
            self.last_position = span.end();
        }
    }

    fn visit_stmt(&mut self, kind: StatementKind, span: Span, is_last: bool) {
        match kind {
            StatementKind::Expression(expr) => self.visit_expr(
                expr,
                if is_last { ExpressionType::SubExpression } else { ExpressionType::Statement },
            ),
            StatementKind::Semi(expr) => {
                self.visit_expr(expr, ExpressionType::Statement);
                self.push_str(";");
            }
            StatementKind::Let(let_stmt) => {
                let let_str = self.slice(span.start()..let_stmt.expression.span.start()).trim_end();

                let expr_str = rewrite::sub_expr(self, self.shape(), let_stmt.expression);

                self.push_rewrite(format!("{let_str} {expr_str};"), span);
            }
            StatementKind::Constrain(ConstrainStatement { kind, arguments, span: _ }) => {
                let mut nested_shape = self.shape();
                let shape = nested_shape;

                nested_shape.indent.block_indent(self.config);

                let callee = match kind {
                    ConstrainKind::Assert | ConstrainKind::Constrain => "assert",
                    ConstrainKind::AssertEq => "assert_eq",
                };
                let args = arguments
                    .into_iter()
                    .map(|arg| rewrite::sub_expr(self, nested_shape, arg))
                    .collect::<Vec<String>>()
                    .join(", ");

                let args = wrap_exprs(
                    "(",
                    ")",
                    args,
                    nested_shape,
                    shape,
                    NewlineMode::IfContainsNewLineAndWidth,
                );
                let constrain = format!("{callee}{args};");

                self.push_rewrite(constrain, span);
            }
            StatementKind::For(for_stmt) => {
                let identifier = self.slice(for_stmt.identifier.span());
                let range = match for_stmt.range {
                    ForRange::Range(start, end) => format!(
                        "{}..{}",
                        rewrite::sub_expr(self, self.shape(), start),
                        rewrite::sub_expr(self, self.shape(), end)
                    ),
                    ForRange::Array(array) => rewrite::sub_expr(self, self.shape(), array),
                };
                let block = rewrite::sub_expr(self, self.shape(), for_stmt.block);

                let result = format!("for {identifier} in {range} {block}");
                self.push_rewrite(result, span);
            }
            StatementKind::Assign(_) => {
                self.push_rewrite(self.slice(span).to_string(), span);
            }
            StatementKind::Error => unreachable!(),
            StatementKind::Break => self.push_rewrite("break;".into(), span),
            StatementKind::Continue => self.push_rewrite("continue;".into(), span),
            StatementKind::Comptime(statement) => self.visit_stmt(statement.kind, span, is_last),
            StatementKind::Interned(_) => unreachable!(
                "StatementKind::Resolved should only emitted by the comptime interpreter"
            ),
        }
    }
}

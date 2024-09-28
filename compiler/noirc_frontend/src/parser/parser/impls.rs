use noirc_errors::Span;

use crate::{
    ast::{
        Documented, Expression, ExpressionKind, GenericTypeArgs, Ident, ItemVisibility,
        NoirFunction, NoirTraitImpl, Path, TraitImplItem, TraitImplItemKind, TypeImpl,
        UnresolvedGeneric, UnresolvedType, UnresolvedTypeData,
    },
    parser::ParserErrorReason,
    token::Keyword,
};

use super::Parser;

pub(crate) enum Impl {
    Impl(TypeImpl),
    TraitImpl(NoirTraitImpl),
}

impl<'a> Parser<'a> {
    pub(crate) fn parse_impl(&mut self) -> Impl {
        let generics = self.parse_generics();

        let type_span_start = self.current_token_span;
        let object_type = self.parse_type_or_error();
        let type_span = self.span_since(type_span_start);

        if self.eat_keyword(Keyword::For) {
            if let UnresolvedTypeData::Named(trait_name, trait_generics, _) = object_type.typ {
                return Impl::TraitImpl(self.parse_trait_impl(
                    generics,
                    trait_generics,
                    trait_name,
                ));
            } else {
                // TODO: error, but we continue parsing the type and assume this is going to be a regular impl
                self.parse_type();
            };
        }

        let where_clause = self.parse_where_clause();
        let methods = self.parse_impl_body();

        Impl::Impl(TypeImpl { object_type, type_span, generics, where_clause, methods })
    }

    fn parse_impl_body(&mut self) -> Vec<(Documented<NoirFunction>, Span)> {
        let mut methods = Vec::new();

        if !self.eat_left_brace() {
            // TODO: error
            return methods;
        }

        loop {
            // TODO: maybe require visibility to always come first
            let doc_comments = self.parse_outer_doc_comments();
            let start_span = self.current_token_span;
            let modifiers = self.parse_modifiers();
            let attributes = Vec::new();

            if self.eat_keyword(Keyword::Fn) {
                let method = self.parse_function(
                    attributes,
                    modifiers.visibility,
                    modifiers.comptime.is_some(),
                    modifiers.unconstrained.is_some(),
                    true, // allow_self
                );
                methods.push((Documented::new(method, doc_comments), self.span_since(start_span)));

                if self.eat_right_brace() {
                    break;
                }
            } else {
                // TODO: parse Type and Constant
                // TODO: error if visibility, unconstrained or comptime were found

                if !self.eat_right_brace() {
                    // TODO: error
                }

                break;
            }
        }

        methods
    }

    fn parse_trait_impl(
        &mut self,
        impl_generics: Vec<UnresolvedGeneric>,
        trait_generics: GenericTypeArgs,
        trait_name: Path,
    ) -> NoirTraitImpl {
        let object_type = self.parse_type_or_error();
        let where_clause = self.parse_where_clause();
        let items = self.parse_trait_impl_items();

        NoirTraitImpl {
            impl_generics,
            trait_name,
            trait_generics,
            object_type,
            where_clause,
            items,
        }
    }

    fn parse_trait_impl_items(&mut self) -> Vec<Documented<TraitImplItem>> {
        let mut items = Vec::new();

        if !self.eat_left_brace() {
            // TODO: error
            return items;
        }

        loop {
            // TODO: maybe require visibility to always come first
            let start_span = self.current_token_span;
            let doc_comments = self.parse_outer_doc_comments();

            if let Some(kind) = self.parse_trait_impl_item_kind() {
                let item = TraitImplItem { kind, span: self.span_since(start_span) };
                items.push(Documented::new(item, doc_comments));

                if self.eat_right_brace() {
                    break;
                }
            } else {
                // TODO: error
                if self.is_eof() || self.eat_right_brace() {
                    break;
                } else {
                    // Keep going
                    self.next_token();
                }
            }
        }

        items
    }

    fn parse_trait_impl_item_kind(&mut self) -> Option<TraitImplItemKind> {
        if let Some(kind) = self.parse_trait_impl_type() {
            return Some(kind);
        }

        if let Some(kind) = self.parse_trait_impl_function() {
            return Some(kind);
        }

        if let Some(kind) = self.parse_trait_impl_constant() {
            return Some(kind);
        }

        None
    }

    fn parse_trait_impl_function(&mut self) -> Option<TraitImplItemKind> {
        let modifiers = self.parse_modifiers();
        if modifiers.visibility != ItemVisibility::Private {
            self.push_error(
                ParserErrorReason::TraitImplVisibilityIgnored,
                modifiers.visibility_span,
            );
        }
        let attributes = Vec::new();

        if !self.eat_keyword(Keyword::Fn) {
            // TODO: error if unconstrained, visibility or comptime
            return None;
        }

        let noir_function = self.parse_function(
            attributes,
            ItemVisibility::Public,
            modifiers.comptime.is_some(),
            modifiers.unconstrained.is_some(),
            true, // allow_self
        );
        Some(TraitImplItemKind::Function(noir_function))
    }

    fn parse_trait_impl_type(&mut self) -> Option<TraitImplItemKind> {
        if !self.eat_keyword(Keyword::Type) {
            return None;
        }

        let Some(name) = self.eat_ident() else {
            // TODO: error
            self.eat_semicolons();
            return Some(TraitImplItemKind::Type {
                name: Ident::default(),
                alias: UnresolvedType { typ: UnresolvedTypeData::Error, span: Span::default() },
            });
        };

        let alias = if self.eat_assign() {
            self.parse_type_or_error()
        } else {
            UnresolvedType { typ: UnresolvedTypeData::Error, span: Span::default() }
        };

        self.eat_semicolons();

        Some(TraitImplItemKind::Type { name, alias })
    }

    fn parse_trait_impl_constant(&mut self) -> Option<TraitImplItemKind> {
        if !self.eat_keyword(Keyword::Let) {
            return None;
        }

        let name = match self.eat_ident() {
            Some(name) => name,
            None => {
                // TODO: error
                Ident::default()
            }
        };

        let typ = if self.eat_colon() {
            self.parse_type_or_error()
        } else {
            UnresolvedType { typ: UnresolvedTypeData::Unspecified, span: Span::default() }
        };

        let expr = if self.eat_assign() {
            self.parse_expression_or_error()
        } else {
            // TODO: error
            Expression { kind: ExpressionKind::Error, span: Span::default() }
        };

        self.eat_semicolons();

        Some(TraitImplItemKind::Constant(name, typ, expr))
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        ast::{ItemVisibility, Pattern, TraitImplItemKind, UnresolvedTypeData},
        parser::{parser::parse_program, ItemKind},
    };

    #[test]
    fn parse_empty_impl() {
        let src = "impl Foo {}";
        let (module, errors) = parse_program(src);
        assert!(errors.is_empty());
        assert_eq!(module.items.len(), 1);
        let item = &module.items[0];
        let ItemKind::Impl(type_impl) = &item.kind else {
            panic!("Expected type impl");
        };
        assert_eq!(type_impl.object_type.to_string(), "Foo");
        assert!(type_impl.generics.is_empty());
        assert!(type_impl.methods.is_empty());
    }

    #[test]
    fn parse_empty_impl_with_generics() {
        let src = "impl <A, B> Foo {}";
        let (module, errors) = parse_program(src);
        assert!(errors.is_empty());
        assert_eq!(module.items.len(), 1);
        let item = &module.items[0];
        let ItemKind::Impl(type_impl) = &item.kind else {
            panic!("Expected type impl");
        };
        assert_eq!(type_impl.object_type.to_string(), "Foo");
        assert_eq!(type_impl.generics.len(), 2);
        assert!(type_impl.methods.is_empty());
    }

    #[test]
    fn parse_impl_with_methods() {
        let src = "impl Foo { unconstrained fn foo() {} pub comptime fn bar() {} }";
        let (mut module, errors) = parse_program(src);
        assert!(errors.is_empty());
        assert_eq!(module.items.len(), 1);
        let item = module.items.remove(0);
        let ItemKind::Impl(mut type_impl) = item.kind else {
            panic!("Expected type impl");
        };
        assert_eq!(type_impl.object_type.to_string(), "Foo");
        assert_eq!(type_impl.methods.len(), 2);

        let (method, _) = type_impl.methods.remove(0);
        let method = method.item;
        assert_eq!(method.def.name.to_string(), "foo");
        assert!(method.def.is_unconstrained);
        assert!(!method.def.is_comptime);
        assert_eq!(method.def.visibility, ItemVisibility::Private);

        let (method, _) = type_impl.methods.remove(0);
        let method = method.item;
        assert_eq!(method.def.name.to_string(), "bar");
        assert!(method.def.is_comptime);
        assert_eq!(method.def.visibility, ItemVisibility::Public);
    }

    #[test]
    fn parse_impl_with_self_argument() {
        let src = "impl Foo { fn foo(self) {} }";
        let (mut module, errors) = parse_program(src);
        assert!(errors.is_empty());
        assert_eq!(module.items.len(), 1);
        let item = module.items.remove(0);
        let ItemKind::Impl(mut type_impl) = item.kind else {
            panic!("Expected type impl");
        };
        assert_eq!(type_impl.methods.len(), 1);

        let (method, _) = type_impl.methods.remove(0);
        let mut method = method.item;
        assert_eq!(method.def.name.to_string(), "foo");
        assert_eq!(method.def.parameters.len(), 1);

        let param = method.def.parameters.remove(0);
        let Pattern::Identifier(name) = param.pattern else {
            panic!("Expected identifier pattern");
        };
        assert_eq!(name.to_string(), "self");
        assert_eq!(param.typ.to_string(), "Self");
    }

    #[test]
    fn parse_impl_with_mut_self_argument() {
        let src = "impl Foo { fn foo(mut self) {} }";
        let (mut module, errors) = parse_program(src);
        assert!(errors.is_empty());
        assert_eq!(module.items.len(), 1);
        let item = module.items.remove(0);
        let ItemKind::Impl(mut type_impl) = item.kind else {
            panic!("Expected type impl");
        };
        assert_eq!(type_impl.methods.len(), 1);

        let (method, _) = type_impl.methods.remove(0);
        let mut method = method.item;
        assert_eq!(method.def.name.to_string(), "foo");
        assert_eq!(method.def.parameters.len(), 1);

        let param = method.def.parameters.remove(0);
        let Pattern::Mutable(pattern, _, true) = param.pattern else {
            panic!("Expected mutable pattern");
        };
        let pattern: &Pattern = &pattern;
        let Pattern::Identifier(name) = pattern else {
            panic!("Expected identifier pattern");
        };
        assert_eq!(name.to_string(), "self");
        assert_eq!(param.typ.to_string(), "Self");
    }

    #[test]
    fn parse_impl_with_reference_mut_self_argument() {
        let src = "impl Foo { fn foo(&mut self) {} }";
        let (mut module, errors) = parse_program(src);
        assert!(errors.is_empty());
        assert_eq!(module.items.len(), 1);
        let item = module.items.remove(0);
        let ItemKind::Impl(mut type_impl) = item.kind else {
            panic!("Expected type impl");
        };
        assert_eq!(type_impl.methods.len(), 1);

        let (method, _) = type_impl.methods.remove(0);
        let mut method = method.item;
        assert_eq!(method.def.name.to_string(), "foo");
        assert_eq!(method.def.parameters.len(), 1);

        let param = method.def.parameters.remove(0);
        let Pattern::Identifier(name) = param.pattern else {
            panic!("Expected identifier pattern");
        };
        assert_eq!(name.to_string(), "self");
        assert_eq!(param.typ.to_string(), "&mut Self");
    }

    #[test]
    fn parse_empty_impl_missing_right_brace() {
        let src = "impl Foo {";
        let (module, errors) = parse_program(src);
        assert!(errors.is_empty()); // TODO: there should be an error here
        assert_eq!(module.items.len(), 1);
        let item = &module.items[0];
        let ItemKind::Impl(type_impl) = &item.kind else {
            panic!("Expected type impl");
        };
        assert_eq!(type_impl.object_type.to_string(), "Foo");
    }

    #[test]
    fn parse_empty_impl_incorrect_body() {
        let src = "impl Foo { hello";
        let (module, errors) = parse_program(src);
        assert!(errors.is_empty()); // TODO: there should be errors here
        assert_eq!(module.items.len(), 1);
        let item = &module.items[0];
        let ItemKind::Impl(type_impl) = &item.kind else {
            panic!("Expected type impl");
        };
        assert_eq!(type_impl.object_type.to_string(), "Foo");
    }

    #[test]
    fn parse_empty_trait_impl() {
        let src = "impl Foo for Field {}";
        let (module, errors) = parse_program(src);
        assert!(errors.is_empty());
        assert_eq!(module.items.len(), 1);
        let item = &module.items[0];
        let ItemKind::TraitImpl(trait_impl) = &item.kind else {
            panic!("Expected trait impl");
        };
        assert_eq!(trait_impl.trait_name.to_string(), "Foo");
        assert!(matches!(trait_impl.object_type.typ, UnresolvedTypeData::FieldElement));
        assert!(trait_impl.items.is_empty());
        assert!(trait_impl.impl_generics.is_empty());
    }

    #[test]
    fn parse_empty_trait_impl_with_generics() {
        let src = "impl <T> Foo for Field {}";
        let (module, errors) = parse_program(src);
        assert!(errors.is_empty());
        assert_eq!(module.items.len(), 1);
        let item = &module.items[0];
        let ItemKind::TraitImpl(trait_impl) = &item.kind else {
            panic!("Expected trait impl");
        };
        assert_eq!(trait_impl.trait_name.to_string(), "Foo");
        assert!(matches!(trait_impl.object_type.typ, UnresolvedTypeData::FieldElement));
        assert!(trait_impl.items.is_empty());
        assert_eq!(trait_impl.impl_generics.len(), 1);
    }

    #[test]
    fn parse_trait_impl_with_function() {
        let src = "impl Foo for Field { fn foo() {} }";
        let (mut module, errors) = parse_program(src);
        assert!(errors.is_empty());
        assert_eq!(module.items.len(), 1);
        let item = module.items.remove(0);
        let ItemKind::TraitImpl(mut trait_impl) = item.kind else {
            panic!("Expected trait impl");
        };
        assert_eq!(trait_impl.trait_name.to_string(), "Foo");
        assert_eq!(trait_impl.items.len(), 1);

        let item = trait_impl.items.remove(0).item;
        let TraitImplItemKind::Function(function) = item.kind else {
            panic!("Expected function");
        };
        assert_eq!(function.def.name.to_string(), "foo");
        assert_eq!(function.def.visibility, ItemVisibility::Public);
    }

    #[test]
    fn parse_trait_impl_with_generic_type_args() {
        let src = "impl Foo<i32, X = Field> for Field { }";
        let (mut module, errors) = parse_program(src);
        assert!(errors.is_empty());
        assert_eq!(module.items.len(), 1);
        let item = module.items.remove(0);
        let ItemKind::TraitImpl(trait_impl) = item.kind else {
            panic!("Expected trait impl");
        };
        assert_eq!(trait_impl.trait_name.to_string(), "Foo");
        assert!(!trait_impl.trait_generics.is_empty());
    }

    #[test]
    fn parse_trait_impl_with_type() {
        let src = "impl Foo for Field { type Foo = i32; }";
        let (mut module, errors) = parse_program(src);
        assert!(errors.is_empty());
        assert_eq!(module.items.len(), 1);
        let item = module.items.remove(0);
        let ItemKind::TraitImpl(mut trait_impl) = item.kind else {
            panic!("Expected trait impl");
        };
        assert_eq!(trait_impl.trait_name.to_string(), "Foo");
        assert_eq!(trait_impl.items.len(), 1);

        let item = trait_impl.items.remove(0).item;
        let TraitImplItemKind::Type { name, alias } = item.kind else {
            panic!("Expected type");
        };
        assert_eq!(name.to_string(), "Foo");
        assert_eq!(alias.to_string(), "i32");
    }

    #[test]
    fn parse_trait_impl_with_let() {
        let src = "impl Foo for Field { let x: Field = 1; }";
        let (mut module, errors) = parse_program(src);
        assert!(errors.is_empty());
        assert_eq!(module.items.len(), 1);
        let item = module.items.remove(0);
        let ItemKind::TraitImpl(mut trait_impl) = item.kind else {
            panic!("Expected trait impl");
        };
        assert_eq!(trait_impl.trait_name.to_string(), "Foo");
        assert_eq!(trait_impl.items.len(), 1);

        let item = trait_impl.items.remove(0).item;
        let TraitImplItemKind::Constant(name, typ, expr) = item.kind else {
            panic!("Expected constant");
        };
        assert_eq!(name.to_string(), "x");
        assert_eq!(typ.to_string(), "Field");
        assert_eq!(expr.to_string(), "1");
    }
}

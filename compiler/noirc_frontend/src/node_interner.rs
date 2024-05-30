use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{hash_map::DefaultHasher, HashMap};
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::Deref;
use std::rc::Rc;

use fm::FileId;
use iter_extended::vecmap;
use noirc_arena::{Arena, Index};
use noirc_errors::{Location, Span, Spanned};
use petgraph::algo::tarjan_scc;
use petgraph::prelude::DiGraph;
use petgraph::prelude::NodeIndex as PetGraphIndex;

use crate::ast::Ident;
use crate::graph::CrateId;
use crate::hir::def_collector::dc_crate::CompilationError;
use crate::hir::def_collector::dc_crate::{UnresolvedStruct, UnresolvedTrait, UnresolvedTypeAlias};
use crate::hir::def_map::{LocalModuleId, ModuleId};

use crate::ast::{BinaryOpKind, FunctionDefinition, ItemVisibility};
use crate::hir::resolution::errors::ResolverError;
use crate::hir_def::stmt::HirLetStatement;
use crate::hir_def::traits::TraitImpl;
use crate::hir_def::traits::{Trait, TraitConstraint};
use crate::hir_def::types::{StructType, Type};
use crate::hir_def::{
    expr::HirExpression,
    function::{FuncMeta, HirFunction},
    stmt::HirStatement,
};
use crate::token::{Attributes, SecondaryAttribute};
use crate::{
    BinaryTypeOperator, Generics, Shared, TypeAlias, TypeBinding, TypeBindings, TypeVariable, TypeVariableId, TypeVariableKind,
};

/// An arbitrary number to limit the recursion depth when searching for trait impls.
/// This is needed to stop recursing for cases such as `impl<T> Foo for T where T: Eq`
const IMPL_SEARCH_RECURSION_LIMIT: u32 = 10;

type StructAttributes = Vec<SecondaryAttribute>;

/// The node interner is the central storage location of all nodes in Noir's Hir (the
/// various node types can be found in hir_def). The interner is also used to collect
/// extra information about the Hir, such as the type of each node, information about
/// each definition or struct, etc. Because it is used on the Hir, the NodeInterner is
/// useful in passes where the Hir is used - name resolution, type checking, and
/// monomorphization - and it is not useful afterward.
#[derive(Debug)]
pub struct NodeInterner {
    pub(crate) nodes: Arena<Node>,
    pub(crate) func_meta: HashMap<FuncId, FuncMeta>,

    function_definition_ids: HashMap<FuncId, DefinitionId>,

    // For a given function ID, this gives the function's modifiers which includes
    // its visibility and whether it is unconstrained, among other information.
    // Unlike func_meta, this map is filled out during definition collection rather than name resolution.
    function_modifiers: HashMap<FuncId, FunctionModifiers>,

    // Contains the source module each function was defined in
    function_modules: HashMap<FuncId, ModuleId>,

    /// This graph tracks dependencies between different global definitions.
    /// This is used to ensure the absense of dependency cycles for globals and types.
    dependency_graph: DiGraph<DependencyId, ()>,

    /// To keep track of where each DependencyId is in `dependency_graph`, we need
    /// this separate graph to map between the ids and indices.
    dependency_graph_indices: HashMap<DependencyId, PetGraphIndex>,

    // Map each `Index` to it's own location
    pub(crate) id_to_location: HashMap<Index, Location>,

    // Maps each DefinitionId to a DefinitionInfo.
    definitions: Vec<DefinitionInfo>,

    // Type checking map
    //
    // This should only be used with indices from the `nodes` arena.
    // Otherwise the indices used may overwrite other existing indices.
    // Each type for each index is filled in during type checking.
    id_to_type: HashMap<Index, Type>,

    // Similar to `id_to_type` but maps definitions to their type
    definition_to_type: HashMap<DefinitionId, Type>,

    // Struct map.
    //
    // Each struct definition is possibly shared across multiple type nodes.
    // It is also mutated through the RefCell during name resolution to append
    // methods from impls to the type.
    structs: HashMap<StructId, Shared<StructType>>,

    struct_attributes: HashMap<StructId, StructAttributes>,

    // Maps TypeAliasId -> Shared<TypeAlias>
    //
    // Map type aliases to the actual type.
    // When resolving types, check against this map to see if a type alias is defined.
    pub(crate) type_aliases: Vec<Shared<TypeAlias>>,

    // Trait map.
    //
    // Each trait definition is possibly shared across multiple type nodes.
    // It is also mutated through the RefCell during name resolution to append
    // methods from impls to the type.
    pub(crate) traits: HashMap<TraitId, Trait>,

    // Trait implementation map
    // For each type that implements a given Trait ( corresponding TraitId), there should be an entry here
    // The purpose for this hashmap is to detect duplication of trait implementations ( if any )
    //
    // Indexed by TraitImplIds
    pub(crate) trait_implementations: Vec<Shared<TraitImpl>>,

    /// Trait implementations on each type. This is expected to always have the same length as
    /// `self.trait_implementations`.
    ///
    /// For lack of a better name, this maps a trait id and type combination
    /// to a corresponding impl if one is available for the type. Due to generics,
    /// we cannot map from Type directly to impl, we need to iterate a Vec of all impls
    /// of that trait to see if any type may match. This can be further optimized later
    /// by splitting it up by type.
    trait_implementation_map: HashMap<TraitId, Vec<(Type, TraitImplKind)>>,

    /// When impls are found during type checking, we tag the function call's Ident
    /// with the impl that was selected. For cases with where clauses, this may be
    /// an Assumed (but verified) impl. In this case the monomorphizer should have
    /// the context to get the concrete type of the object and select the correct impl itself.
    selected_trait_implementations: HashMap<ExprId, TraitImplKind>,

    /// Holds the trait ids of the traits used for operator overloading
    operator_traits: HashMap<BinaryOpKind, TraitId>,

    /// The `Ordering` type is a semi-builtin type that is the result of the comparison traits.
    ordering_type: Option<Type>,

    /// Map from ExprId (referring to a Function/Method call) to its corresponding TypeBindings,
    /// filled out during type checking from instantiated variables. Used during monomorphization
    /// to map call site types back onto function parameter types, and undo this binding as needed.
    instantiation_bindings: HashMap<ExprId, TypeBindings>,

    /// Remembers the field index a given HirMemberAccess expression was resolved to during type
    /// checking.
    field_indices: HashMap<ExprId, usize>,

    // Maps GlobalId -> GlobalInfo
    // NOTE: currently only used for checking repeat globals and restricting their scope to a module
    globals: Vec<GlobalInfo>,
    global_attributes: HashMap<GlobalId, Vec<SecondaryAttribute>>,

    // TODO: rename to arith_expressions
    // arithmetic_expressions: HashMap<ArithId, (ArithExpr, Location)>,
    arith_expressions: HashMap<ArithId, (ArithExpr, Location)>,

    pub(crate) arith_constraints: ArithConstraints,

    next_type_variable_id: std::cell::Cell<usize>,

    /// A map from a struct type and method name to a function id for the method.
    /// This can resolve to potentially multiple methods if the same method name is
    /// specialized for different generics on the same type. E.g. for `Struct<T>`, we
    /// may have both `impl Struct<u32> { fn foo(){} }` and `impl Struct<u8> { fn foo(){} }`.
    /// If this happens, the returned Vec will have 2 entries and we'll need to further
    /// disambiguate them by checking the type of each function.
    struct_methods: HashMap<(StructId, String), Methods>,

    /// Methods on primitive types defined in the stdlib.
    primitive_methods: HashMap<(TypeMethodKey, String), Methods>,

    // For trait implementation functions, this is their self type and trait they belong to
    func_id_to_trait: HashMap<FuncId, (Type, TraitId)>,

    /// A list of all type aliases that are referenced in the program.
    /// Searched by LSP to resolve [Location]s of [TypeAliasType]s
    pub(crate) type_alias_ref: Vec<(TypeAliasId, Location)>,

    /// Stores the [Location] of a [Type] reference
    pub(crate) type_ref_locations: Vec<(Type, Location)>,
}


// TODO: relocate
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ArithId {
    Dummy,
    // TODO cleanup
    // Incremental(usize),
    Hash(u64),
}


// TODO: relocate
// TODO: rename
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Hash, Default)]
pub struct GenericIndex(usize);

impl GenericIndex {
    fn offset(&self, offset_amount: usize) -> Self {
        GenericIndex(self.0 + offset_amount)
    }
}

// TODO: relocate
// TODO: docs
#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub enum ArithExpr {
    Op {
        kind: ArithOpKind,
        lhs: Box<ArithExpr>,
        rhs: Box<ArithExpr>,
    },
    Variable(TypeVariable, Rc<String>, GenericIndex),
    Constant(u64),
}

impl ArithExpr {

    // // TODO: resolve variables given function
    // pub fn resolve(&mut self, _f: F) {
    //     unimplemented!();
    // }

    pub fn try_constant(&self) -> Option<u64> {
        match self {
            Self::Constant(x) => Some(*x),
            _ => None,
        }
    }

    // TODO: replace HashMap with a Vec
    pub fn evaluate(&self, arguments: &HashMap<GenericIndex, u64>) -> Result<u64, ArithExprError> {
        match self {
            Self::Op { kind, lhs, rhs } => {
                let lhs = lhs.evaluate(arguments)?;
                let rhs = rhs.evaluate(arguments)?;
                kind.evaluate(lhs, rhs)
            }
            Self::Variable(binding, name, index) => {
                if let Some(result) = arguments.get(index) {
                    Ok(*result)
                } else {
                    Err(ArithExprError::UnboundVariable {
                        binding: binding.clone(),
                        name: name.to_string(),
                    })
                }
            }
            Self::Constant(result) => Ok(*result),
        }
    }

    /// Apply Type::follow_bindings to each named generic
    /// and return the updated version as well as any new generics
    fn follow_bindings(&self, interner: &NodeInterner, offset_amount: &mut usize) -> (Self, Vec<Type>) {
        match self {
            Self::Op { kind, lhs, rhs } => {
                let (lhs, mut lhs_new_generics) = lhs.follow_bindings(interner, offset_amount);
                let (rhs, mut rhs_new_generics) = rhs.follow_bindings(interner, offset_amount);
                let rhs = rhs.offset_generic_indices(lhs_new_generics.len());
                lhs_new_generics.append(&mut rhs_new_generics);
                (Self::Op { kind: *kind, lhs: Box::new(lhs), rhs: Box::new(rhs) }, lhs_new_generics)
            }
            Self::Variable(binding, name, index) => {
                match Type::NamedGeneric(binding.clone(), name.clone()).follow_bindings() {
                    // TODO: nested case currently unimplemented
                    Type::GenericArith(arith_id, generics) => {
                        let (arith_expr, _location) = interner.get_arithmetic_expression(arith_id);
                        let arith_expr = arith_expr.offset_generic_indices(*offset_amount);
                        *offset_amount = arith_expr.max_generic_index().0;
                        (arith_expr, generics)
                    }

                    Type::NamedGeneric(new_binding, new_name) => (Self::Variable(new_binding, new_name, *index), vec![]),
                    Type::TypeVariable(new_binding, TypeVariableKind::Constant(value)) => (Self::Constant(value), vec![]),
                    Type::TypeVariable(new_binding, kind) => {
                        let new_name = format!("#implicit_var_{:?}_{:?}", new_binding, kind);
                        let new_index = GenericIndex(*offset_amount);
                        *offset_amount += 1;
                        (Self::Variable(new_binding.clone(), new_name.into(), new_index), vec![Type::TypeVariable(new_binding, kind)])
                    }
                    other => panic!("ICE: follow_bindings on Type::NamedGeneric produced a result other than a variable or constant: {:?}", other),
                }
            }
            Self::Constant(result) => (Self::Constant(*result), vec![]),
        }
    }

    /// map over Self::Variable's
    fn map_variables<F>(&self, f: &mut F) -> Self
    where
        F: FnMut(&TypeVariable, &Rc<String>, GenericIndex) -> (TypeVariable, Rc<String>, GenericIndex),
    {
        match self {
            Self::Op { kind, lhs, rhs } => {
                let lhs = Box::new(lhs.map_variables(f));
                let rhs = Box::new(rhs.map_variables(f));
                Self::Op { kind: *kind, lhs, rhs }
            }
            Self::Variable(binding, name, index) => {
                let (new_binding, new_name, new_index) = f(binding, name, *index);
                Self::Variable(new_binding, new_name, new_index)
            }
            Self::Constant(result) => Self::Constant(*result),
        }
    }

    /// normal form: sort nodes at each branch
    fn nf(&self) -> Self {
        match self {
            Self::Op { kind, lhs, rhs } => {
                match kind {
                    // commutative cases
                    ArithOpKind::Add | ArithOpKind::Mul => {
                        let mut lhs_rhs = vec![lhs.nf(), rhs.nf()];
                        lhs_rhs.sort_by(|x, y| {
                            let id_x = x.to_id();
                            let id_y = y.to_id();
                            id_x.cmp(&id_y)
                        });
                        let [ref lhs, ref rhs] = lhs_rhs[..] else { panic!("two element list produced a different number of elements when sorted") };
                        return Self::Op { kind: *kind, lhs: Box::new(lhs.clone()), rhs: Box::new(rhs.clone()) };
                    }
                    _ => (),
                }
                Self::Op { kind: *kind, lhs: lhs.clone(), rhs: rhs.clone() }
            }
            other => other.clone(),
        }
    }

    pub(crate) fn to_id(&self) -> ArithId {
        let mut hasher = DefaultHasher::new();
        self.hash(&mut hasher);
        ArithId::Hash(hasher.finish())
    }

    pub(crate) fn offset_generic_indices(&self, offset_amount: usize) -> Self {
        match self {
            Self::Op { kind, lhs, rhs } => {
                let lhs = Box::new(lhs.offset_generic_indices(offset_amount));
                let rhs = Box::new(rhs.offset_generic_indices(offset_amount));
                Self::Op { kind: *kind, lhs, rhs }
            }
            Self::Variable(binding, name, index) => {
                Self::Variable(binding.clone(), name.clone(), index.offset(offset_amount))
            }
            Self::Constant(result) => Self::Constant(*result),
        }
    }

    pub(crate) fn max_generic_index(&self) -> GenericIndex {
        match self {
            Self::Op { kind, lhs, rhs } => {
                let lhs_max = lhs.max_generic_index();
                let rhs_max = rhs.max_generic_index();
                std::cmp::max(lhs_max, rhs_max)
            }
            Self::Variable(binding, name, index) => *index,
            Self::Constant(result) => GenericIndex::default(),
        }
    }
}

// TODO: relocate
impl std::fmt::Display for ArithExpr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArithExpr::Op { kind, lhs, rhs } => write!(f, "{lhs} {kind} {rhs}"),
            ArithExpr::Variable(binding, name, _index) => match &*binding.borrow() {
                TypeBinding::Bound(binding) => binding.fmt(f),
                TypeBinding::Unbound(_) if name.is_empty() => write!(f, "_"),
                TypeBinding::Unbound(_) => write!(f, "{name}"),
            },
            ArithExpr::Constant(x) => x.fmt(f),
        }
    }
}

// TODO: relocate
// TODO: docs
#[derive(Debug, PartialEq, Eq, Copy, Clone, Hash)]
pub enum ArithOpKind {
    Mul,
    Add,
    Sub,
}

impl ArithOpKind {
    /// Returns an error on overflow/underflow
    pub fn evaluate(&self, x: u64, y: u64) -> Result<u64, ArithExprError> {
        match self {
            Self::Mul => Ok(x * y),
            Self::Add => Ok(x + y),
            Self::Sub => x.checked_sub(y).ok_or_else(|| ArithExprError::SubUnderflow {
                lhs: x,
                rhs: y,
            }),
        }
    }

    pub fn from_binary_type_operator(value: BinaryTypeOperator) -> Option<Self> {
        match value {
            BinaryTypeOperator::Addition => Some(ArithOpKind::Add),
            BinaryTypeOperator::Multiplication => Some(ArithOpKind::Mul),
            BinaryTypeOperator::Subtraction => Some(ArithOpKind::Sub),
            _ => None,
        }
    }
}

// TODO: relocate
impl std::fmt::Display for ArithOpKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArithOpKind::Mul => write!(f, "*"),
            ArithOpKind::Add => write!(f, "+"),
            ArithOpKind::Sub => write!(f, "-"),
        }
    }
}

// TODO: relocate + add proper messages
#[derive(Debug, thiserror::Error)]
pub enum ArithExprError {
    SubUnderflow {
        lhs: u64,
        rhs: u64,
    },

    UnboundVariable {
        binding: TypeVariable,
        name: String,
    },

    EvaluateUnexpectedType {
        unexpected_type: Type,
    }
}

impl std::fmt::Display for ArithExprError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::SubUnderflow { lhs, rhs } => {
                write!(f, "subtracting {} - {} underflowed", lhs, rhs)
            }
            Self::UnboundVariable { binding, name } => {
                if let TypeBinding::Unbound(_) = &*binding.borrow() {
                    write!(f, "unbound variable when resolving generic arithmetic: {}", name)
                } else {
                    write!(f, "unbound variable when resolving generic arithmetic: {}", binding.borrow())
                }
            }
            Self::EvaluateUnexpectedType { unexpected_type } => {
                write!(f, "unexpected type when evaluating to u64: {}", unexpected_type)
            }
        }
    }
}


// TODO: relocate
/// Whether either the LHS or RHS of an ArithConstraint needs to be interned,
/// which can happen when unifying
#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub enum NeedsInterning {
    Lhs(ArithExpr),
    Rhs(ArithExpr),
    Neither,
}

// TODO: relocate
// TODO: docs
#[derive(Debug, PartialEq, Eq, Clone, Hash)]
pub struct ArithConstraint {
    pub lhs: ArithId,
    pub lhs_generics: Vec<Type>,
    pub rhs: ArithId,
    pub rhs_generics: Vec<Type>,
    pub needs_interning: NeedsInterning,
}

impl ArithConstraint {

    // TODO: relocate to ArithExpr
    pub(crate) fn evaluate_generics_to_u64(generics: &Vec<Type>, location: &Location, interner: &NodeInterner) -> Result<HashMap<GenericIndex, u64>, ArithExprError> {
        // TODO: put the inner type variable in as well and unify once it's looked up to ensure
        // they match
        //
        // TODO: cloned needed still?
        generics.iter().cloned().enumerate().map(|(index, generic)| {
            generic.evaluate_to_u64(location, interner).map(|result| {
                (GenericIndex(index), result)
            })
        }).collect::<Result<HashMap<_,_>, _>>()
    }

    // TODO: better errors
    pub fn validate(&self, interner: &NodeInterner) -> Result<(), ArithConstraintError> {

        // TODO: cleanup
        dbg!("validating", self);

        // TODO: so many clones needed? (get_arithmetic_expression returns a reference)
        let (lhs, rhs) = match &self.needs_interning {
            NeedsInterning::Lhs(lhs_expr) =>
                ((lhs_expr.clone(), Location::dummy()), interner.get_arithmetic_expression(self.rhs).clone()),
            NeedsInterning::Rhs(rhs_expr) =>
                (interner.get_arithmetic_expression(self.lhs).clone(), (rhs_expr.clone(), Location::dummy())),
            NeedsInterning::Neither =>
                (interner.get_arithmetic_expression(self.lhs).clone(), interner.get_arithmetic_expression(self.rhs).clone()),
        };
        let (lhs_expr, lhs_location) = lhs;
        let (rhs_expr, rhs_location) = rhs;

        dbg!("validating: pre-follow_bindings", &lhs_expr, &rhs_expr);

        // follow NamedGeneric bindings
        let mut current_generic_index_offset = 0;
        let (lhs_expr, lhs_new_generics) = lhs_expr.follow_bindings(interner, &mut current_generic_index_offset);
        let (rhs_expr, rhs_new_generics) = rhs_expr.follow_bindings(interner, &mut current_generic_index_offset);
        rhs_expr.offset_generic_indices(lhs_new_generics.len());

        // TODO: remove cloned
        let lhs_generics: Vec<_> = self.lhs_generics.iter().cloned().chain(lhs_new_generics.into_iter()).collect();
        let rhs_generics: Vec<_> = self.rhs_generics.iter().cloned().chain(rhs_new_generics.into_iter()).collect();

        dbg!("validating: post-follow_bindings", &lhs_expr, &rhs_expr);

        dbg!("validating: loading", &self.lhs_generics, &self.rhs_generics);
        match Self::evaluate_generics_to_u64(&lhs_generics, &lhs_location, interner).and_then(|lhs_generics| {
            let rhs_generics = Self::evaluate_generics_to_u64(&rhs_generics, &rhs_location, interner)?;
            Ok((lhs_generics, rhs_generics))
        }) {
            // all generics resolved
            Ok((lhs_generics, rhs_generics)) => {

                // TODO: cleanup
                dbg!("validating: loaded", &lhs_expr, &lhs_generics, &rhs_expr, &rhs_generics);

                match (lhs_expr.evaluate(&lhs_generics), rhs_expr.evaluate(&rhs_generics)) {
                    (Ok(lhs_evaluated), Ok(rhs_evaluated)) => {
                        if lhs_evaluated == rhs_evaluated {
                            // TODO: cleanup
                            dbg!("validating: evaluated", &lhs_evaluated, &rhs_evaluated);

                            Ok(())
                        } else {
                            Err(ArithConstraintError::EvaluatedToDifferentValues { lhs_evaluated, rhs_evaluated, location: rhs_location, other_location: lhs_location })
                        }
                    }
                    (lhs_expr, rhs_expr) => {
                        Err(ArithConstraintError::FailedToEvaluate { lhs_expr, rhs_expr, location: lhs_location, other_location: rhs_location })
                    }
                }

            }
            Err(arith_expr_error) => {
                dbg!("validating: fallback", &self.lhs_generics, &self.rhs_generics, &lhs_expr, &rhs_expr);

                // let mut fresh_bindings = type_bindings.clone();
                let mut fresh_bindings = TypeBindings::new();

                let generics_match = self.lhs_generics.iter()
                    // TODO: remove clone?
                    .zip(self.rhs_generics.clone())
                    .all(|(lhs_generic, rhs_generic)| {
                        // TODO: are there bindings to instantiate for this type?
                        // let rhs_generic = rhs_generic.substitute(&interner.instantiation_bindings);
                        lhs_generic.try_unify(&rhs_generic, &mut fresh_bindings, &interner.arith_constraints).is_ok()
                });

                Type::apply_type_bindings(fresh_bindings);

                if generics_match {
                    dbg!("generics_match");

                    // get all generics from lhs, rhs
                    //     - ensure that name overlaps iff typevariable overlaps

                    // let id_to_generic: HashMap<GenericIndex, TypeVariable> =
                    //     self.lhs_generics.iter().map(|lhs_generic| {
                    //         let var = lhs_generic.get_outer_type_variable()
                    //             .expect("all args to GenericArith to be NamedGeneric/TypeVariable's");
                    //         (var.id(), var)
                    //     }).collect();

                    let lhs_expr = lhs_expr.map_variables(&mut |_var: &TypeVariable, name: &Rc<String>, index: GenericIndex| {
                        let new_var = self.lhs_generics
                            .get(index.0)
                            // .get(&var.id())
                            // TODO: better error needed?
                            .expect("all variables in a GenericArith ArithExpr to be in the included Vec")
                            .get_outer_type_variable()
                            .expect("all args to GenericArith to be NamedGeneric/TypeVariable's");
                        (new_var, name.clone(), index)
                    }).nf();

                    let rhs_expr = rhs_expr.map_variables(&mut |_var: &TypeVariable, name: &Rc<String>, index: GenericIndex| {
                        let new_var = self.lhs_generics
                            .get(index.0)
                            // .get(&var.id())
                            // TODO: better error needed?
                            .expect("all variables in a GenericArith ArithExpr to be in the included Vec")
                            .get_outer_type_variable()
                            .expect("all args to GenericArith to be NamedGeneric/TypeVariable's");
                        (new_var, name.clone(), index)
                    }).nf();

                    // let rhs_expr = rhs_expr.map_variables(&mut |_, name: &Rc<String>| {
                    //     let var = lhs_name_to_generic
                    //         .get(name)
                    //         // TODO: better error needed?
                    //         .expect("all variables in a GenericArith ArithExpr to be in the included Vec");
                    //     (var.clone(), name.clone())
                    // });

                    if lhs_expr == rhs_expr {
                        Ok(())
                    } else {
                        Err(ArithConstraintError::DistinctExpressions {
                            lhs_expr: lhs_expr.clone(),
                            rhs_expr: rhs_expr.clone(),
                            generics: self.lhs_generics.clone(),
                            location: lhs_location,
                            other_location: rhs_location
                        })
                    }

                } else {
                    // unresolved generics are preventing resolution
                    Err(ArithConstraintError::ArithExprError { arith_expr_error, location: lhs_location, other_locations: vec![rhs_location] })
                }
            }
        }

    }
}

pub type ArithConstraints = RefCell<Vec<ArithConstraint>>;


// TODO relocate/cleanup
// TODO add proper messages
#[derive(Debug, thiserror::Error)]
pub enum ArithConstraintError {
    UnresolvedGeneric { generic: Type, location: Location },
    EvaluatedToDifferentValues { lhs_evaluated: u64, rhs_evaluated: u64, location: Location, other_location: Location },
    FailedToEvaluate { lhs_expr: Result<u64, ArithExprError>, rhs_expr: Result<u64, ArithExprError>, location: Location, other_location: Location },
    DistinctExpressions { lhs_expr: ArithExpr, rhs_expr: ArithExpr, generics: Vec<Type>, location: Location, other_location: Location },
    ArithExprError { arith_expr_error: ArithExprError, location: Location, other_locations: Vec<Location> },
}

impl std::fmt::Display for ArithConstraintError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::UnresolvedGeneric { generic, .. } => {
                // TODO: better error message
                if let Type::NamedGeneric(_, name) = generic {
                    write!(f, "Unresolved generic value: {}", name)
                } else {
                    write!(f, "Unresolved generic value: {}", generic)
                }
            }
            Self::EvaluatedToDifferentValues { lhs_evaluated, rhs_evaluated, .. } => {
                // TODO: better error message
                write!(f, "Generic arithmetic evaluated to different values: {} != {}", lhs_evaluated, rhs_evaluated)
            }
            Self::FailedToEvaluate { lhs_expr, rhs_expr, .. } => {
                // TODO: better error message (this prints Result's)
                write!(f, "Generic arithmetic evaluated differently: {:?} != {:?}", lhs_expr, rhs_expr)
            }
            Self::DistinctExpressions { lhs_expr, rhs_expr, generics, .. } => {
                // TODO: better error message (this prints Result's)
                // TODO: pretty print ArithExpr's
                write!(f, "Generic arithmetic appears to be distinct: {:?} != {:?}, where the arguments are: {:?}", lhs_expr, rhs_expr, generics)
            }
            Self::ArithExprError { arith_expr_error, .. } => arith_expr_error.fmt(f),
        }
    }
}



impl ArithConstraintError {
    pub fn location(&self) -> Location {
        match self {
            Self::UnresolvedGeneric { location, .. }
            | Self::EvaluatedToDifferentValues { location, .. }
            | Self::FailedToEvaluate { location, .. }
            | Self::DistinctExpressions { location, .. }
            | Self::ArithExprError { location, .. } => *location,
        }
    }

    pub fn other_locations(&self) -> Vec<Location> {
        match self {
            Self::UnresolvedGeneric { .. } => vec![],

            Self::EvaluatedToDifferentValues { other_location, .. }
            | Self::FailedToEvaluate { other_location, .. }
            | Self::DistinctExpressions { other_location, .. } => vec![*other_location],

            Self::ArithExprError { other_locations, .. } => other_locations.clone(),
        }
    }
}



/// A dependency in the dependency graph may be a type or a definition.
/// Types can depend on definitions too. E.g. `Foo` depends on `COUNT` in:
///
/// ```struct
/// global COUNT = 3;
///
/// struct Foo {
///     array: [Field; COUNT],
/// }
/// ```
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub enum DependencyId {
    Struct(StructId),
    Global(GlobalId),
    Function(FuncId),
    Alias(TypeAliasId),
}

/// A trait implementation is either a normal implementation that is present in the source
/// program via an `impl` block, or it is assumed to exist from a `where` clause or similar.
#[derive(Debug, Clone)]
pub enum TraitImplKind {
    Normal(TraitImplId),

    /// Assumed impls don't have an impl id since they don't link back to any concrete part of the source code.
    Assumed {
        object_type: Type,

        /// The trait generics to use - if specified.
        /// This is allowed to be empty when they are inferred. E.g. for:
        ///
        /// ```
        /// trait Into<T> {
        ///     fn into(self) -> T;
        /// }
        /// ```
        ///
        /// The reference `Into::into(x)` would have inferred generics, but
        /// `x.into()` with a `X: Into<Y>` in scope would not.
        trait_generics: Vec<Type>,
    },
}

/// Represents the methods on a given type that each share the same name.
///
/// Methods are split into inherent methods and trait methods. If there is
/// ever a name that is defined on both a type directly, and defined indirectly
/// via a trait impl, the direct (inherent) name will always take precedence.
///
/// Additionally, types can define specialized impls with methods of the same name
/// as long as these specialized impls do not overlap. E.g. `impl Struct<u32>` and `impl Struct<u64>`
#[derive(Default, Debug, Clone)]
pub struct Methods {
    pub direct: Vec<FuncId>,
    pub trait_impl_methods: Vec<FuncId>,
}

/// All the information from a function that is filled out during definition collection rather than
/// name resolution. As a result, if information about a function is needed during name resolution,
/// this is the only place where it is safe to retrieve it (where all fields are guaranteed to be initialized).
#[derive(Debug, Clone)]
pub struct FunctionModifiers {
    pub name: String,

    /// Whether the function is `pub` or not.
    pub visibility: ItemVisibility,

    pub attributes: Attributes,

    pub is_unconstrained: bool,

    pub is_comptime: bool,
}

impl FunctionModifiers {
    /// A semi-reasonable set of default FunctionModifiers used for testing.
    #[cfg(test)]
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            name: String::new(),
            visibility: ItemVisibility::Public,
            attributes: Attributes::empty(),
            is_unconstrained: false,
            is_comptime: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub struct DefinitionId(usize);

impl DefinitionId {
    //dummy id for error reporting
    pub fn dummy_id() -> DefinitionId {
        DefinitionId(std::usize::MAX)
    }
}

/// An ID for a global value
#[derive(Debug, Eq, PartialEq, Hash, Clone, Copy)]
pub struct GlobalId(usize);

impl GlobalId {
    // Dummy id for error reporting
    pub fn dummy_id() -> Self {
        GlobalId(std::usize::MAX)
    }
}

#[derive(Debug, Eq, PartialEq, Hash, Clone, Copy)]
pub struct StmtId(Index);

impl StmtId {
    //dummy id for error reporting
    // This can be anything, as the program will ultimately fail
    // after resolution
    pub fn dummy_id() -> StmtId {
        StmtId(Index::dummy())
    }
}

#[derive(Debug, Eq, PartialEq, Hash, Copy, Clone)]
pub struct ExprId(Index);

impl ExprId {
    pub fn empty_block_id() -> ExprId {
        ExprId(Index::unsafe_zeroed())
    }
}
#[derive(Debug, Eq, PartialEq, Hash, Copy, Clone)]
pub struct FuncId(Index);

impl FuncId {
    //dummy id for error reporting
    // This can be anything, as the program will ultimately fail
    // after resolution
    pub fn dummy_id() -> FuncId {
        FuncId(Index::dummy())
    }
}

impl fmt::Display for FuncId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Eq, PartialEq, Hash, Copy, Clone, PartialOrd, Ord)]
pub struct StructId(ModuleId);

impl StructId {
    //dummy id for error reporting
    // This can be anything, as the program will ultimately fail
    // after resolution
    pub fn dummy_id() -> StructId {
        StructId(ModuleId { krate: CrateId::dummy_id(), local_id: LocalModuleId::dummy_id() })
    }

    pub fn module_id(self) -> ModuleId {
        self.0
    }

    pub fn krate(self) -> CrateId {
        self.0.krate
    }

    pub fn local_module_id(self) -> LocalModuleId {
        self.0.local_id
    }
}

#[derive(Debug, Eq, PartialEq, Hash, Copy, Clone, PartialOrd, Ord)]
pub struct TypeAliasId(pub usize);

impl TypeAliasId {
    pub fn dummy_id() -> TypeAliasId {
        TypeAliasId(std::usize::MAX)
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TraitId(pub ModuleId);

impl TraitId {
    // dummy id for error reporting
    // This can be anything, as the program will ultimately fail
    // after resolution
    pub fn dummy_id() -> TraitId {
        TraitId(ModuleId { krate: CrateId::dummy_id(), local_id: LocalModuleId::dummy_id() })
    }
}

#[derive(Debug, Eq, PartialEq, Hash, Clone, Copy)]
pub struct TraitImplId(pub usize);

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TraitMethodId {
    pub trait_id: TraitId,
    pub method_index: usize, // index in Trait::methods
}

macro_rules! into_index {
    ($id_type:ty) => {
        impl From<$id_type> for Index {
            fn from(t: $id_type) -> Self {
                t.0
            }
        }

        impl From<&$id_type> for Index {
            fn from(t: &$id_type) -> Self {
                t.0
            }
        }
    };
}

into_index!(ExprId);
into_index!(StmtId);

/// A Definition enum specifies anything that we can intern in the NodeInterner
/// We use one Arena for all types that can be interned as that has better cache locality
/// This data structure is never accessed directly, so API wise there is no difference between using
/// Multiple arenas and a single Arena
#[derive(Debug, Clone)]
pub(crate) enum Node {
    Function(HirFunction),
    Statement(HirStatement),
    Expression(HirExpression),
}

#[derive(Debug, Clone)]
pub struct DefinitionInfo {
    pub name: String,
    pub mutable: bool,
    pub kind: DefinitionKind,
    pub location: Location,
}

impl DefinitionInfo {
    /// True if this definition is for a global variable.
    /// Note that this returns false for top-level functions.
    pub fn is_global(&self) -> bool {
        self.kind.is_global()
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum DefinitionKind {
    Function(FuncId),

    Global(GlobalId),

    /// Locals may be defined in let statements or parameters,
    /// in which case they will not have an associated ExprId
    Local(Option<ExprId>),

    /// Generic types in functions (T, U in `fn foo<T, U>(...)` are declared as variables
    /// in scope in case they resolve to numeric generics later.
    GenericType(TypeVariable),
}

impl DefinitionKind {
    /// True if this definition is for a global variable.
    /// Note that this returns false for top-level functions.
    pub fn is_global(&self) -> bool {
        matches!(self, DefinitionKind::Global(..))
    }

    pub fn get_rhs(&self) -> Option<ExprId> {
        match self {
            DefinitionKind::Function(_) => None,
            DefinitionKind::Global(_) => None,
            DefinitionKind::Local(id) => *id,
            DefinitionKind::GenericType(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GlobalInfo {
    pub id: GlobalId,
    pub definition_id: DefinitionId,
    pub ident: Ident,
    pub local_id: LocalModuleId,
    pub location: Location,
    pub let_statement: StmtId,
}

impl Default for NodeInterner {
    fn default() -> Self {
        let mut interner = NodeInterner {
            nodes: Arena::default(),
            func_meta: HashMap::new(),
            function_definition_ids: HashMap::new(),
            function_modifiers: HashMap::new(),
            function_modules: HashMap::new(),
            func_id_to_trait: HashMap::new(),
            dependency_graph: petgraph::graph::DiGraph::new(),
            dependency_graph_indices: HashMap::new(),
            id_to_location: HashMap::new(),
            definitions: vec![],
            id_to_type: HashMap::new(),
            definition_to_type: HashMap::new(),
            structs: HashMap::new(),
            struct_attributes: HashMap::new(),
            type_aliases: Vec::new(),
            traits: HashMap::new(),
            trait_implementations: Vec::new(),
            trait_implementation_map: HashMap::new(),
            selected_trait_implementations: HashMap::new(),
            operator_traits: HashMap::new(),
            ordering_type: None,
            instantiation_bindings: HashMap::new(),
            field_indices: HashMap::new(),
            next_type_variable_id: std::cell::Cell::new(0),
            globals: Vec::new(),
            global_attributes: HashMap::new(),
            arith_expressions: HashMap::new(),
            arith_constraints: Vec::new().into(),
            struct_methods: HashMap::new(),
            primitive_methods: HashMap::new(),
            type_alias_ref: Vec::new(),
            type_ref_locations: Vec::new(),
        };

        // An empty block expression is used often, we add this into the `node` on startup
        let expr_id = interner.push_expr(HirExpression::empty_block());
        assert_eq!(expr_id, ExprId::empty_block_id());
        interner
    }
}

// XXX: Add check that insertions are not overwrites for maps
// XXX: Maybe change push to intern, and remove comments
impl NodeInterner {
    /// Interns a HIR statement.
    pub fn push_stmt(&mut self, stmt: HirStatement) -> StmtId {
        StmtId(self.nodes.insert(Node::Statement(stmt)))
    }
    /// Interns a HIR expression.
    pub fn push_expr(&mut self, expr: HirExpression) -> ExprId {
        ExprId(self.nodes.insert(Node::Expression(expr)))
    }

    /// Stores the span for an interned expression.
    pub fn push_expr_location(&mut self, expr_id: ExprId, span: Span, file: FileId) {
        self.id_to_location.insert(expr_id.into(), Location::new(span, file));
    }

    /// Interns a HIR Function.
    pub fn push_fn(&mut self, func: HirFunction) -> FuncId {
        FuncId(self.nodes.insert(Node::Function(func)))
    }

    /// Store the type for an interned expression
    pub fn push_expr_type(&mut self, expr_id: ExprId, typ: Type) {
        self.id_to_type.insert(expr_id.into(), typ);
    }

    /// Store the type for a definition
    pub fn push_definition_type(&mut self, definition_id: DefinitionId, typ: Type) {
        self.definition_to_type.insert(definition_id, typ);
    }

    pub fn push_empty_trait(&mut self, type_id: TraitId, unresolved_trait: &UnresolvedTrait) {
        let self_type_typevar_id = self.next_type_variable_id();

        let new_trait = Trait {
            id: type_id,
            name: unresolved_trait.trait_def.name.clone(),
            crate_id: unresolved_trait.crate_id,
            location: Location::new(unresolved_trait.trait_def.span, unresolved_trait.file_id),
            generics: vecmap(&unresolved_trait.trait_def.generics, |_| {
                // Temporary type variable ids before the trait is resolved to its actual ids.
                // This lets us record how many arguments the type expects so that other types
                // can refer to it with generic arguments before the generic parameters themselves
                // are resolved.
                TypeVariable::unbound(TypeVariableId(0))
            }),
            self_type_typevar_id,
            self_type_typevar: TypeVariable::unbound(self_type_typevar_id),
            methods: Vec::new(),
            method_ids: unresolved_trait.method_ids.clone(),
            constants: Vec::new(),
            types: Vec::new(),
        };

        self.traits.insert(type_id, new_trait);
    }

    pub fn new_struct(
        &mut self,
        typ: &UnresolvedStruct,
        krate: CrateId,
        local_id: LocalModuleId,
        file_id: FileId,
    ) -> StructId {
        let struct_id = StructId(ModuleId { krate, local_id });
        let name = typ.struct_def.name.clone();

        // Fields will be filled in later
        let no_fields = Vec::new();
        let generics = vecmap(&typ.struct_def.generics, |_| {
            // Temporary type variable ids before the struct is resolved to its actual ids.
            // This lets us record how many arguments the type expects so that other types
            // can refer to it with generic arguments before the generic parameters themselves
            // are resolved.
            TypeVariable::unbound(TypeVariableId(0))
        });

        let location = Location::new(typ.struct_def.span, file_id);
        let new_struct = StructType::new(struct_id, name, location, no_fields, generics);
        self.structs.insert(struct_id, Shared::new(new_struct));
        self.struct_attributes.insert(struct_id, typ.struct_def.attributes.clone());
        struct_id
    }

    pub fn push_type_alias(&mut self, typ: &UnresolvedTypeAlias) -> TypeAliasId {
        let type_id = TypeAliasId(self.type_aliases.len());

        self.type_aliases.push(Shared::new(TypeAlias::new(
            type_id,
            typ.type_alias_def.name.clone(),
            Location::new(typ.type_alias_def.span, typ.file_id),
            Type::Error,
            vecmap(&typ.type_alias_def.generics, |_| TypeVariable::unbound(TypeVariableId(0))),
        )));

        type_id
    }

    /// Adds [TypeLiasId] and [Location] to the type_alias_ref vector
    /// So that we can later resolve [Location]s type aliases from the LSP requests
    pub fn add_type_alias_ref(&mut self, type_id: TypeAliasId, location: Location) {
        self.type_alias_ref.push((type_id, location));
    }
    pub fn update_struct(&mut self, type_id: StructId, f: impl FnOnce(&mut StructType)) {
        let mut value = self.structs.get_mut(&type_id).unwrap().borrow_mut();
        f(&mut value);
    }

    pub fn update_trait(&mut self, trait_id: TraitId, f: impl FnOnce(&mut Trait)) {
        let value = self.traits.get_mut(&trait_id).unwrap();
        f(value);
    }

    pub fn set_type_alias(&mut self, type_id: TypeAliasId, typ: Type, generics: Generics) {
        let type_alias_type = &mut self.type_aliases[type_id.0];
        type_alias_type.borrow_mut().set_type_and_generics(typ, generics);
    }

    /// Returns the interned statement corresponding to `stmt_id`
    pub fn update_statement(&mut self, stmt_id: &StmtId, f: impl FnOnce(&mut HirStatement)) {
        let def =
            self.nodes.get_mut(stmt_id.0).expect("ice: all statement ids should have definitions");

        match def {
            Node::Statement(stmt) => f(stmt),
            _ => panic!("ice: all statement ids should correspond to a statement in the interner"),
        }
    }

    /// Updates the interned expression corresponding to `expr_id`
    pub fn update_expression(&mut self, expr_id: ExprId, f: impl FnOnce(&mut HirExpression)) {
        let def =
            self.nodes.get_mut(expr_id.0).expect("ice: all expression ids should have definitions");

        match def {
            Node::Expression(expr) => f(expr),
            _ => {
                panic!("ice: all expression ids should correspond to a expression in the interner")
            }
        }
    }

    /// Store [Location] of [Type] reference
    pub fn push_type_ref_location(&mut self, typ: Type, location: Location) {
        self.type_ref_locations.push((typ, location));
    }

    fn push_global(
        &mut self,
        ident: Ident,
        local_id: LocalModuleId,
        let_statement: StmtId,
        file: FileId,
        attributes: Vec<SecondaryAttribute>,
        mutable: bool,
    ) -> GlobalId {
        let id = GlobalId(self.globals.len());
        let location = Location::new(ident.span(), file);
        let name = ident.to_string();
        let definition_id =
            self.push_definition(name, mutable, DefinitionKind::Global(id), location);

        self.globals.push(GlobalInfo {
            id,
            definition_id,
            ident,
            local_id,
            let_statement,
            location,
        });
        self.global_attributes.insert(id, attributes);
        id
    }

    pub fn next_global_id(&mut self) -> GlobalId {
        GlobalId(self.globals.len())
    }

    /// Intern an empty global. Used for collecting globals before they're defined
    pub fn push_empty_global(
        &mut self,
        name: Ident,
        local_id: LocalModuleId,
        file: FileId,
        attributes: Vec<SecondaryAttribute>,
        mutable: bool,
    ) -> GlobalId {
        let statement = self.push_stmt(HirStatement::Error);
        let span = name.span();
        let id = self.push_global(name, local_id, statement, file, attributes, mutable);
        self.push_stmt_location(statement, span, file);
        id
    }

    // TODO rename to push_arith_expression
    pub fn push_arithmetic_expression(&mut self, expr: ArithExpr, location: Location) -> ArithId {
        let arith_id = expr.to_id();
        self.arith_expressions.insert(arith_id, (expr, location)); 
        arith_id
    }

    // TODO: rename to get_arith_expression
    pub fn get_arithmetic_expression(&self, arith_id: ArithId) -> &(ArithExpr, Location) {
        self.arith_expressions.get(&arith_id)
            .expect(&*format!("ICE: unknown ArithId ({:?})\n\n{:?}", arith_id, self.arith_expressions))
    }

    // TODO cleanup
    // pub fn next_arith_id(&self) -> ArithId {
    //     ArithId::Incremental(self.arithmetic_expressions.len())
    // }

    /// Intern an empty function.
    pub fn push_empty_fn(&mut self) -> FuncId {
        self.push_fn(HirFunction::empty())
    }
    /// Updates the underlying interned Function.
    ///
    /// This method is used as we eagerly intern empty functions to
    /// generate function identifiers and then we update at a later point in
    /// time.
    pub fn update_fn(&mut self, func_id: FuncId, hir_func: HirFunction) {
        let def =
            self.nodes.get_mut(func_id.0).expect("ice: all function ids should have definitions");

        let func = match def {
            Node::Function(func) => func,
            _ => panic!("ice: all function ids should correspond to a function in the interner"),
        };
        *func = hir_func;
    }

    pub fn find_function(&self, function_name: &str) -> Option<FuncId> {
        self.func_meta
            .iter()
            .find(|(func_id, _func_meta)| self.function_name(func_id) == function_name)
            .map(|(func_id, _meta)| *func_id)
    }

    ///Interns a function's metadata.
    ///
    /// Note that the FuncId has been created already.
    /// See ModCollector for it's usage.
    pub fn push_fn_meta(&mut self, func_data: FuncMeta, func_id: FuncId) {
        self.func_meta.insert(func_id, func_data);
    }

    pub fn push_definition(
        &mut self,
        name: String,
        mutable: bool,
        definition: DefinitionKind,
        location: Location,
    ) -> DefinitionId {
        let id = DefinitionId(self.definitions.len());
        if let DefinitionKind::Function(func_id) = definition {
            self.function_definition_ids.insert(func_id, id);
        }

        self.definitions.push(DefinitionInfo { name, mutable, kind: definition, location });
        id
    }

    /// Push a function with the default modifiers and [`ModuleId`] for testing
    #[cfg(test)]
    pub fn push_test_function_definition(&mut self, name: String) -> FuncId {
        let id = self.push_fn(HirFunction::empty());
        let mut modifiers = FunctionModifiers::new();
        modifiers.name = name;
        let module = ModuleId::dummy_id();
        let location = Location::dummy();
        self.push_function_definition(id, modifiers, module, location);
        id
    }

    pub fn push_function(
        &mut self,
        id: FuncId,
        function: &FunctionDefinition,
        module: ModuleId,
        location: Location,
    ) -> DefinitionId {
        let modifiers = FunctionModifiers {
            name: function.name.0.contents.clone(),
            visibility: function.visibility,
            attributes: function.attributes.clone(),
            is_unconstrained: function.is_unconstrained,
            is_comptime: function.is_comptime,
        };
        self.push_function_definition(id, modifiers, module, location)
    }

    pub fn push_function_definition(
        &mut self,
        func: FuncId,
        modifiers: FunctionModifiers,
        module: ModuleId,
        location: Location,
    ) -> DefinitionId {
        let name = modifiers.name.clone();
        self.function_modifiers.insert(func, modifiers);
        self.function_modules.insert(func, module);
        self.push_definition(name, false, DefinitionKind::Function(func), location)
    }

    pub fn set_function_trait(&mut self, func: FuncId, self_type: Type, trait_id: TraitId) {
        self.func_id_to_trait.insert(func, (self_type, trait_id));
    }

    pub fn get_function_trait(&self, func: &FuncId) -> Option<(Type, TraitId)> {
        self.func_id_to_trait.get(func).cloned()
    }

    /// Returns the visibility of the given function.
    ///
    /// The underlying function_visibilities map is populated during def collection,
    /// so this function can be called anytime afterward.
    pub fn function_visibility(&self, func: FuncId) -> ItemVisibility {
        self.function_modifiers[&func].visibility
    }

    /// Returns the module this function was defined within
    pub fn function_module(&self, func: FuncId) -> ModuleId {
        self.function_modules[&func]
    }

    /// Returns the interned HIR function corresponding to `func_id`
    //
    // Cloning HIR structures is cheap, so we return owned structures
    pub fn function(&self, func_id: &FuncId) -> HirFunction {
        let def = self.nodes.get(func_id.0).expect("ice: all function ids should have definitions");

        match def {
            Node::Function(func) => func.clone(),
            _ => panic!("ice: all function ids should correspond to a function in the interner"),
        }
    }

    /// Returns the interned meta data corresponding to `func_id`
    pub fn function_meta(&self, func_id: &FuncId) -> &FuncMeta {
        self.func_meta.get(func_id).expect("ice: all function ids should have metadata")
    }

    pub fn try_function_meta(&self, func_id: &FuncId) -> Option<&FuncMeta> {
        self.func_meta.get(func_id)
    }

    pub fn function_ident(&self, func_id: &FuncId) -> crate::ast::Ident {
        let name = self.function_name(func_id).to_owned();
        let span = self.function_meta(func_id).name.location.span;
        crate::ast::Ident(Spanned::from(span, name))
    }

    pub fn function_name(&self, func_id: &FuncId) -> &str {
        &self.function_modifiers[func_id].name
    }

    pub fn function_modifiers(&self, func_id: &FuncId) -> &FunctionModifiers {
        &self.function_modifiers[func_id]
    }

    pub fn function_modifiers_mut(&mut self, func_id: &FuncId) -> &mut FunctionModifiers {
        self.function_modifiers.get_mut(func_id).expect("func_id should always have modifiers")
    }

    pub fn function_attributes(&self, func_id: &FuncId) -> &Attributes {
        &self.function_modifiers[func_id].attributes
    }

    pub fn struct_attributes(&self, struct_id: &StructId) -> &StructAttributes {
        &self.struct_attributes[struct_id]
    }

    pub fn global_attributes(&self, global_id: &GlobalId) -> &[SecondaryAttribute] {
        &self.global_attributes[global_id]
    }

    /// Returns the interned statement corresponding to `stmt_id`
    pub fn statement(&self, stmt_id: &StmtId) -> HirStatement {
        let def =
            self.nodes.get(stmt_id.0).expect("ice: all statement ids should have definitions");

        match def {
            Node::Statement(stmt) => stmt.clone(),
            _ => panic!("ice: all statement ids should correspond to a statement in the interner"),
        }
    }

    /// Try to get the `HirLetStatement` which defines a given global value
    pub fn get_global_let_statement(&self, global: GlobalId) -> Option<HirLetStatement> {
        let global = self.get_global(global);
        let def = self.nodes.get(global.let_statement.0)?;

        match def {
            Node::Statement(hir_stmt) => match hir_stmt {
                HirStatement::Let(let_stmt) => Some(let_stmt.clone()),
                _ => {
                    panic!("ice: all globals should correspond to a let statement in the interner")
                }
            },
            _ => panic!("ice: all globals should correspond to a statement in the interner"),
        }
    }

    /// Returns the interned expression corresponding to `expr_id`
    pub fn expression(&self, expr_id: &ExprId) -> HirExpression {
        let def =
            self.nodes.get(expr_id.0).expect("ice: all expression ids should have definitions");

        match def {
            Node::Expression(expr) => expr.clone(),
            _ => {
                panic!("ice: all expression ids should correspond to a expression in the interner")
            }
        }
    }

    /// Retrieves the definition where the given id was defined.
    /// This will panic if given DefinitionId::dummy_id. Use try_definition for
    /// any call with a possibly undefined variable.
    pub fn definition(&self, id: DefinitionId) -> &DefinitionInfo {
        &self.definitions[id.0]
    }

    /// Tries to retrieve the given id's definition.
    /// This function should be used during name resolution or type checking when we cannot be sure
    /// all variables have corresponding definitions (in case of an error in the user's code).
    pub fn try_definition(&self, id: DefinitionId) -> Option<&DefinitionInfo> {
        self.definitions.get(id.0)
    }

    /// Returns the name of the definition
    ///
    /// This is needed as the Environment needs to map variable names to witness indices
    pub fn definition_name(&self, id: DefinitionId) -> &str {
        &self.definition(id).name
    }

    pub fn expr_span(&self, expr_id: &ExprId) -> Span {
        self.id_location(expr_id).span
    }

    pub fn expr_location(&self, expr_id: &ExprId) -> Location {
        self.id_location(expr_id)
    }

    pub fn statement_span(&self, stmt_id: StmtId) -> Span {
        self.id_location(stmt_id).span
    }

    pub fn statement_location(&self, stmt_id: StmtId) -> Location {
        self.id_location(stmt_id)
    }

    pub fn push_stmt_location(&mut self, id: StmtId, span: Span, file: FileId) {
        self.id_to_location.insert(id.into(), Location::new(span, file));
    }

    pub fn get_struct(&self, id: StructId) -> Shared<StructType> {
        self.structs[&id].clone()
    }

    pub fn get_struct_methods(&self, id: StructId) -> Vec<Methods> {
        self.struct_methods
            .keys()
            .filter_map(|(key_id, name)| {
                if key_id == &id {
                    Some(
                        self.struct_methods
                            .get(&(*key_id, name.clone()))
                            .expect("get_struct_methods given invalid StructId")
                            .clone(),
                    )
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn get_trait(&self, id: TraitId) -> &Trait {
        &self.traits[&id]
    }

    pub fn get_trait_mut(&mut self, id: TraitId) -> &mut Trait {
        self.traits.get_mut(&id).expect("get_trait_mut given invalid TraitId")
    }

    pub fn try_get_trait(&self, id: TraitId) -> Option<&Trait> {
        self.traits.get(&id)
    }

    pub fn get_type_alias(&self, id: TypeAliasId) -> Shared<TypeAlias> {
        self.type_aliases[id.0].clone()
    }

    pub fn get_global(&self, global_id: GlobalId) -> &GlobalInfo {
        &self.globals[global_id.0]
    }

    pub fn get_global_definition(&self, global_id: GlobalId) -> &DefinitionInfo {
        let global = self.get_global(global_id);
        self.definition(global.definition_id)
    }

    pub fn get_all_globals(&self) -> &[GlobalInfo] {
        &self.globals
    }

    /// Returns the type of an item stored in the Interner or Error if it was not found.
    pub fn id_type(&self, index: impl Into<Index>) -> Type {
        self.id_to_type.get(&index.into()).cloned().unwrap_or(Type::Error)
    }

    /// Returns the type of the definition or `Type::Error` if it was not found.
    pub fn definition_type(&self, id: DefinitionId) -> Type {
        self.definition_to_type.get(&id).cloned().unwrap_or(Type::Error)
    }

    pub fn id_type_substitute_trait_as_type(&self, def_id: DefinitionId) -> Type {
        let typ = self.definition_type(def_id);
        if let Type::Function(args, ret, env) = &typ {
            let def = self.definition(def_id);
            if let Type::TraitAsType(..) = ret.as_ref() {
                if let DefinitionKind::Function(func_id) = def.kind {
                    let f = self.function(&func_id);
                    let func_body = f.as_expr();
                    let ret_type = self.id_type(func_body);
                    let new_type = Type::Function(args.clone(), Box::new(ret_type), env.clone());
                    return new_type;
                }
            }
        }
        typ
    }

    /// Returns the span of an item stored in the Interner
    pub fn id_location(&self, index: impl Into<Index>) -> Location {
        self.id_to_location.get(&index.into()).copied().unwrap()
    }

    /// Replaces the HirExpression at the given ExprId with a new HirExpression
    pub fn replace_expr(&mut self, id: &ExprId, new: HirExpression) {
        let old = self.nodes.get_mut(id.into()).unwrap();
        *old = Node::Expression(new);
    }

    /// Replaces the HirStatement at the given StmtId with a new HirStatement
    pub fn replace_statement(&mut self, stmt_id: StmtId, hir_stmt: HirStatement) {
        let old = self.nodes.get_mut(stmt_id.0).unwrap();
        *old = Node::Statement(hir_stmt);
    }

    pub fn next_type_variable_id(&self) -> TypeVariableId {
        let id = self.next_type_variable_id.get();
        self.next_type_variable_id.set(id + 1);
        TypeVariableId(id)
    }

    pub fn next_type_variable(&self) -> Type {
        Type::type_variable(self.next_type_variable_id())
    }

    pub fn store_instantiation_bindings(
        &mut self,
        expr_id: ExprId,
        instantiation_bindings: TypeBindings,
    ) {
        self.instantiation_bindings.insert(expr_id, instantiation_bindings);
    }

    pub fn get_instantiation_bindings(&self, expr_id: ExprId) -> &TypeBindings {
        &self.instantiation_bindings[&expr_id]
    }

    pub fn get_field_index(&self, expr_id: ExprId) -> usize {
        self.field_indices[&expr_id]
    }

    pub fn set_field_index(&mut self, expr_id: ExprId, index: usize) {
        self.field_indices.insert(expr_id, index);
    }

    pub fn function_definition_id(&self, function: FuncId) -> DefinitionId {
        self.function_definition_ids[&function]
    }

    /// Returns the DefinitionId of a trait's method, panics if the given trait method
    /// is not a valid method of the trait or if the trait has not yet had
    /// its methods ids set during name resolution.
    pub fn trait_method_id(&self, trait_method: TraitMethodId) -> DefinitionId {
        let the_trait = self.get_trait(trait_method.trait_id);
        let method_name = &the_trait.methods[trait_method.method_index].name;
        let function_id = the_trait.method_ids[&method_name.0.contents];
        self.function_definition_id(function_id)
    }

    /// Adds a non-trait method to a type.
    ///
    /// Returns `Some(duplicate)` if a matching method was already defined.
    /// Returns `None` otherwise.
    pub fn add_method(
        &mut self,
        self_type: &Type,
        method_name: String,
        method_id: FuncId,
        is_trait_method: bool,
    ) -> Option<FuncId> {
        match self_type {
            Type::Struct(struct_type, _generics) => {
                let id = struct_type.borrow().id;

                if let Some(existing) = self.lookup_method(self_type, id, &method_name, true) {
                    return Some(existing);
                }

                let key = (id, method_name);
                self.struct_methods.entry(key).or_default().add_method(method_id, is_trait_method);
                None
            }
            Type::Error => None,
            Type::MutableReference(element) => {
                self.add_method(element, method_name, method_id, is_trait_method)
            }

            other => {
                let key = get_type_method_key(self_type).unwrap_or_else(|| {
                    unreachable!("Cannot add a method to the unsupported type '{}'", other)
                });
                self.primitive_methods
                    .entry((key, method_name))
                    .or_default()
                    .add_method(method_id, is_trait_method);
                None
            }
        }
    }

    pub fn get_trait_implementation(&self, id: TraitImplId) -> Shared<TraitImpl> {
        self.trait_implementations[id.0].clone()
    }

    /// Given a `ObjectType: TraitId` pair, try to find an existing impl that satisfies the
    /// constraint. If an impl cannot be found, this will return a vector of each constraint
    /// in the path to get to the failing constraint. Usually this is just the single failing
    /// constraint, but when where clauses are involved, the failing constraint may be several
    /// constraints deep. In this case, all of the constraints are returned, starting with the
    /// failing one.
    /// If this list of failing constraints is empty, this means type annotations are required.
    pub fn lookup_trait_implementation(
        &mut self,
        object_type: &Type,
        trait_id: TraitId,
        trait_generics: &[Type],
    ) -> Result<TraitImplKind, Vec<TraitConstraint>> {
        let (impl_kind, bindings) =
            self.try_lookup_trait_implementation(object_type, trait_id, trait_generics)?;

        Type::apply_type_bindings(bindings);
        Ok(impl_kind)
    }

    /// Given a `ObjectType: TraitId` pair, find all implementations without taking constraints into account or
    /// applying any type bindings. Useful to look for a specific trait in a type that is used in a macro.
    pub fn lookup_all_trait_implementations(
        &self,
        object_type: &Type,
        trait_id: TraitId,
    ) -> Vec<&TraitImplKind> {
        let trait_impl = self.trait_implementation_map.get(&trait_id);

        trait_impl
            .map(|trait_impl| {
                trait_impl
                    .iter()
                    .filter_map(|(typ, impl_kind)| match &typ {
                        Type::Forall(_, typ) => {
                            if typ.deref() == object_type {
                                Some(impl_kind)
                            } else {
                                None
                            }
                        }
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Similar to `lookup_trait_implementation` but does not apply any type bindings on success.
    /// On error returns either:
    /// - 1+ failing trait constraints, including the original.
    ///   Each constraint after the first represents a `where` clause that was followed.
    /// - 0 trait constraints indicating type annotations are needed to choose an impl.
    pub fn try_lookup_trait_implementation(
        &mut self,
        object_type: &Type,
        trait_id: TraitId,
        trait_generics: &[Type],
    ) -> Result<(TraitImplKind, TypeBindings), Vec<TraitConstraint>> {
        let mut bindings = TypeBindings::new();
        let impl_kind = self.lookup_trait_implementation_helper(
            object_type,
            trait_id,
            trait_generics,
            &mut bindings,
            IMPL_SEARCH_RECURSION_LIMIT,
        )?;
        Ok((impl_kind, bindings))
    }

    /// Returns the trait implementation if found.
    /// On error returns either:
    /// - 1+ failing trait constraints, including the original.
    ///   Each constraint after the first represents a `where` clause that was followed.
    /// - 0 trait constraints indicating type annotations are needed to choose an impl.
    fn lookup_trait_implementation_helper(
        &self,
        object_type: &Type,
        trait_id: TraitId,
        trait_generics: &[Type],
        type_bindings: &mut TypeBindings,
        recursion_limit: u32,
    ) -> Result<TraitImplKind, Vec<TraitConstraint>> {
        let make_constraint =
            || TraitConstraint::new(object_type.clone(), trait_id, trait_generics.to_vec());

        // Prevent infinite recursion when looking for impls
        if recursion_limit == 0 {
            return Err(vec![make_constraint()]);
        }

        let object_type = object_type.substitute(type_bindings);

        // If the object type isn't known, just return an error saying type annotations are needed.
        if object_type.is_bindable() {
            return Err(Vec::new());
        }

        let impls =
            self.trait_implementation_map.get(&trait_id).ok_or_else(|| vec![make_constraint()])?;

        let mut matching_impls = Vec::new();

        for (existing_object_type2, impl_kind) in impls {
            // Bug: We're instantiating only the object type's generics here, not all of the trait's generics like we need to
            let (existing_object_type, instantiation_bindings) =
                existing_object_type2.instantiate(self);

            let mut fresh_bindings = type_bindings.clone();

            let mut check_trait_generics = |impl_generics: &[Type]| {
                trait_generics.iter().zip(impl_generics).all(|(trait_generic, impl_generic2)| {
                    let impl_generic = impl_generic2.substitute(&instantiation_bindings);
                    trait_generic.try_unify(&impl_generic, &mut fresh_bindings, &self.arith_constraints).is_ok()
                })
            };

            let generics_match = match impl_kind {
                TraitImplKind::Normal(id) => {
                    let shared_impl = self.get_trait_implementation(*id);
                    let shared_impl = shared_impl.borrow();
                    check_trait_generics(&shared_impl.trait_generics)
                }
                TraitImplKind::Assumed { trait_generics, .. } => {
                    check_trait_generics(trait_generics)
                }
            };

            if !generics_match {
                continue;
            }

            if object_type.try_unify(&existing_object_type, &mut fresh_bindings, &self.arith_constraints).is_ok() {
                if let TraitImplKind::Normal(impl_id) = impl_kind {
                    let trait_impl = self.get_trait_implementation(*impl_id);
                    let trait_impl = trait_impl.borrow();

                    if let Err(mut errors) = self.validate_where_clause(
                        &trait_impl.where_clause,
                        &mut fresh_bindings,
                        &instantiation_bindings,
                        recursion_limit,
                    ) {
                        // TODO: cleanup
                        dbg!("lookup_trait_implementation_helper where clause");
                        errors.push(make_constraint());
                        return Err(errors);
                    }
                }

                matching_impls.push((impl_kind.clone(), fresh_bindings));
            }
        }

        if matching_impls.len() == 1 {
            let (impl_, fresh_bindings) = matching_impls.pop().unwrap();
            *type_bindings = fresh_bindings;
            Ok(impl_)
        } else if matching_impls.is_empty() {
            // TODO: cleanup
            dbg!("lookup_trait_implementation_helper no matching impl");
            Err(vec![make_constraint()])
        } else {
            // multiple matching impls, type annotations needed
            Err(vec![])
        }
    }

    /// Verifies that each constraint in the given where clause is valid.
    /// If an impl cannot be found for any constraint, the erroring constraint is returned.
    fn validate_where_clause(
        &self,
        where_clause: &[TraitConstraint],
        type_bindings: &mut TypeBindings,
        instantiation_bindings: &TypeBindings,
        recursion_limit: u32,
    ) -> Result<(), Vec<TraitConstraint>> {
        for constraint in where_clause {
            // Instantiation bindings are generally safe to force substitute into the same type.
            // This is needed here to undo any bindings done to trait methods by monomorphization.
            // Otherwise, an impl for (A, B) could get narrowed to only an impl for e.g. (u8, u16).
            let constraint_type =
                constraint.typ.force_substitute(instantiation_bindings).substitute(type_bindings);

            let trait_generics = vecmap(&constraint.trait_generics, |generic| {
                generic.force_substitute(instantiation_bindings).substitute(type_bindings)
            });

            self.lookup_trait_implementation_helper(
                &constraint_type,
                constraint.trait_id,
                &trait_generics,
                // Use a fresh set of type bindings here since the constraint_type originates from
                // our impl list, which we don't want to bind to.
                type_bindings,
                recursion_limit - 1,
            )?;
        }

        Ok(())
    }

    /// Adds an "assumed" trait implementation to the currently known trait implementations.
    /// Unlike normal trait implementations, these are only assumed to exist. They often correspond
    /// to `where` clauses in functions where we assume there is some `T: Eq` even though we do
    /// not yet know T. For these cases, we store an impl here so that we assume they exist and
    /// can resolve them. They are then later verified when the function is called, and linked
    /// properly after being monomorphized to the correct variant.
    ///
    /// Returns true on success, or false if there is already an overlapping impl in scope.
    pub fn add_assumed_trait_implementation(
        &mut self,
        object_type: Type,
        trait_id: TraitId,
        trait_generics: Vec<Type>,
    ) -> bool {
        // Make sure there are no overlapping impls
        if self.try_lookup_trait_implementation(&object_type, trait_id, &trait_generics).is_ok() {
            return false;
        }

        let entries = self.trait_implementation_map.entry(trait_id).or_default();
        entries.push((object_type.clone(), TraitImplKind::Assumed { object_type, trait_generics }));
        true
    }

    /// Adds a trait implementation to the list of known implementations.
    pub fn add_trait_implementation(
        &mut self,
        object_type: Type,
        trait_id: TraitId,
        trait_generics: Vec<Type>,
        impl_id: TraitImplId,
        impl_generics: Generics,
        trait_impl: Shared<TraitImpl>,
    ) -> Result<(), (Span, FileId)> {
        assert_eq!(impl_id.0, self.trait_implementations.len(), "trait impl defined out of order");

        self.trait_implementations.push(trait_impl.clone());

        // Replace each generic with a fresh type variable
        let substitutions = impl_generics
            .into_iter()
            .map(|typevar| (typevar.id(), (typevar, self.next_type_variable())))
            .collect();

        let instantiated_object_type = object_type.substitute(&substitutions);

        // Ignoring overlapping `TraitImplKind::Assumed` impls here is perfectly fine.
        // It should never happen since impls are defined at global scope, but even
        // if they were, we should never prevent defining a new impl because a 'where'
        // clause already assumes it exists.
        if let Ok((TraitImplKind::Normal(existing), _)) = self.try_lookup_trait_implementation(
            &instantiated_object_type,
            trait_id,
            &trait_generics,
        ) {
            let existing_impl = self.get_trait_implementation(existing);
            let existing_impl = existing_impl.borrow();
            return Err((existing_impl.ident.span(), existing_impl.file));
        }

        for method in &trait_impl.borrow().methods {
            let method_name = self.function_name(method).to_owned();
            self.add_method(&object_type, method_name, *method, true);
        }

        // The object type is generalized so that a generic impl will apply
        // to any type T, rather than just the generic type named T.
        let generalized_object_type = object_type.generalize_from_substitutions(substitutions);

        let entries = self.trait_implementation_map.entry(trait_id).or_default();
        entries.push((generalized_object_type, TraitImplKind::Normal(impl_id)));
        Ok(())
    }

    /// Search by name for a method on the given struct.
    ///
    /// If `check_type` is true, this will force `lookup_method` to check the type
    /// of each candidate instead of returning only the first candidate if there is exactly one.
    /// This is generally only desired when declaring new methods to check if they overlap any
    /// existing methods.
    ///
    /// Another detail is that this method does not handle auto-dereferencing through `&mut T`.
    /// So if an object is of type `self : &mut T` but a method only accepts `self: T` (or
    /// vice-versa), the call will not be selected. If this is ever implemented into this method,
    /// we can remove the `methods.len() == 1` check and the `check_type` early return.
    pub fn lookup_method(
        &self,
        typ: &Type,
        id: StructId,
        method_name: &str,
        force_type_check: bool,
    ) -> Option<FuncId> {
        let methods = self.struct_methods.get(&(id, method_name.to_owned()));
        // If there is only one method, just return it immediately.
        // It will still be typechecked later.
        if !force_type_check {
            if let Some(method) = methods.and_then(|m| m.get_unambiguous()) {
                return Some(method);
            }
        }

        self.find_matching_method(typ, methods, method_name)
    }

    /// Select the 1 matching method with an object type matching `typ`
    fn find_matching_method(
        &self,
        typ: &Type,
        methods: Option<&Methods>,
        method_name: &str,
    ) -> Option<FuncId> {
        if let Some(method) = methods.and_then(|m| m.find_matching_method(typ, self)) {
            Some(method)
        } else {
            // Failed to find a match for the type in question, switch to looking at impls
            // for all types `T`, e.g. `impl<T> Foo for T`
            let key = &(TypeMethodKey::Generic, method_name.to_owned());
            let global_methods = self.primitive_methods.get(key)?.clone();
            global_methods.find_matching_method(typ, self)
        }
    }

    /// Looks up a given method name on the given primitive type.
    pub fn lookup_primitive_method(
        &mut self,
        typ: &Type,
        method_name: &str,
    ) -> Option<FuncId> {
        let key = get_type_method_key(typ)?;
        let methods = self.primitive_methods.get(&(key, method_name.to_owned()))?.clone();
        self.find_matching_method(typ, Some(&methods), method_name)
    }

    pub fn lookup_primitive_trait_method_mut(
        &mut self,
        typ: &Type,
        method_name: &str,
    ) -> Option<FuncId> {
        let typ = Type::MutableReference(Box::new(typ.clone()));
        self.lookup_primitive_method(&typ, method_name)
    }

    /// Returns what the next trait impl id is expected to be.
    /// Note that this does not actually reserve the slot so care should
    /// be taken that the next trait impl added matches this ID.
    pub fn next_trait_impl_id(&self) -> TraitImplId {
        TraitImplId(self.trait_implementations.len())
    }

    /// Removes all TraitImplKind::Assumed from the list of known impls for the given trait
    pub fn remove_assumed_trait_implementations_for_trait(&mut self, trait_id: TraitId) {
        let entries = self.trait_implementation_map.entry(trait_id).or_default();
        entries.retain(|(_, kind)| matches!(kind, TraitImplKind::Normal(_)));
    }

    /// Tags the given identifier with the selected trait_impl so that monomorphization
    /// can later recover which impl was selected, or alternatively see if it needs to
    /// decide which impl to select (because the impl was Assumed).
    pub fn select_impl_for_expression(&mut self, ident_id: ExprId, trait_impl: TraitImplKind) {
        self.selected_trait_implementations.insert(ident_id, trait_impl);
    }

    /// Retrieves the impl selected for a given ExprId during name resolution.
    pub fn get_selected_impl_for_expression(&self, ident_id: ExprId) -> Option<TraitImplKind> {
        self.selected_trait_implementations.get(&ident_id).cloned()
    }

    /// Retrieves the trait id for a given binary operator.
    /// All binary operators correspond to a trait - although multiple may correspond
    /// to the same trait (such as `==` and `!=`).
    /// `self.operator_traits` is expected to be filled before name resolution,
    /// during definition collection.
    pub fn get_operator_trait_method(&self, operator: BinaryOpKind) -> TraitMethodId {
        let trait_id = self.operator_traits[&operator];

        // Assume that the operator's method to be overloaded is the first method of the trait.
        TraitMethodId { trait_id, method_index: 0 }
    }

    /// Add the given trait as an operator trait if its name matches one of the
    /// operator trait names (Add, Sub, ...).
    pub fn try_add_operator_trait(&mut self, trait_id: TraitId) {
        let the_trait = self.get_trait(trait_id);

        let operator = match the_trait.name.0.contents.as_str() {
            "Add" => BinaryOpKind::Add,
            "Sub" => BinaryOpKind::Subtract,
            "Mul" => BinaryOpKind::Multiply,
            "Div" => BinaryOpKind::Divide,
            "Rem" => BinaryOpKind::Modulo,
            "Eq" => BinaryOpKind::Equal,
            "Ord" => BinaryOpKind::Less,
            "BitAnd" => BinaryOpKind::And,
            "BitOr" => BinaryOpKind::Or,
            "BitXor" => BinaryOpKind::Xor,
            "Shl" => BinaryOpKind::ShiftLeft,
            "Shr" => BinaryOpKind::ShiftRight,
            _ => return,
        };

        self.operator_traits.insert(operator, trait_id);

        // Some operators also require we insert a matching entry for related operators
        match operator {
            BinaryOpKind::Equal => {
                self.operator_traits.insert(BinaryOpKind::NotEqual, trait_id);
            }
            BinaryOpKind::Less => {
                self.operator_traits.insert(BinaryOpKind::LessEqual, trait_id);
                self.operator_traits.insert(BinaryOpKind::Greater, trait_id);
                self.operator_traits.insert(BinaryOpKind::GreaterEqual, trait_id);

                let the_trait = self.get_trait(trait_id);
                self.ordering_type = match &the_trait.methods[0].typ {
                    Type::Forall(_, typ) => match typ.as_ref() {
                        Type::Function(_, return_type, _) => Some(return_type.as_ref().clone()),
                        other => unreachable!("Expected function type for `cmp`, found {}", other),
                    },
                    other => unreachable!("Expected Forall type for `cmp`, found {}", other),
                };
            }
            _ => (),
        }
    }

    /// This function is needed when creating a NodeInterner for testing so that calls
    /// to `get_operator_trait` do not panic when the stdlib isn't present.
    #[cfg(test)]
    pub fn populate_dummy_operator_traits(&mut self) {
        let dummy_trait = TraitId(ModuleId::dummy_id());
        self.operator_traits.insert(BinaryOpKind::Add, dummy_trait);
        self.operator_traits.insert(BinaryOpKind::Subtract, dummy_trait);
        self.operator_traits.insert(BinaryOpKind::Multiply, dummy_trait);
        self.operator_traits.insert(BinaryOpKind::Divide, dummy_trait);
        self.operator_traits.insert(BinaryOpKind::Modulo, dummy_trait);
        self.operator_traits.insert(BinaryOpKind::Equal, dummy_trait);
        self.operator_traits.insert(BinaryOpKind::NotEqual, dummy_trait);
        self.operator_traits.insert(BinaryOpKind::Less, dummy_trait);
        self.operator_traits.insert(BinaryOpKind::LessEqual, dummy_trait);
        self.operator_traits.insert(BinaryOpKind::Greater, dummy_trait);
        self.operator_traits.insert(BinaryOpKind::GreaterEqual, dummy_trait);
        self.operator_traits.insert(BinaryOpKind::And, dummy_trait);
        self.operator_traits.insert(BinaryOpKind::Or, dummy_trait);
        self.operator_traits.insert(BinaryOpKind::Xor, dummy_trait);
        self.operator_traits.insert(BinaryOpKind::ShiftLeft, dummy_trait);
        self.operator_traits.insert(BinaryOpKind::ShiftRight, dummy_trait);
    }

    pub(crate) fn ordering_type(&self) -> Type {
        self.ordering_type.clone().expect("Expected ordering_type to be set in the NodeInterner")
    }

    /// Register that `dependent` depends on `dependency`.
    /// This is usually because `dependent` refers to `dependency` in one of its struct fields.
    pub fn add_type_dependency(&mut self, dependent: DependencyId, dependency: StructId) {
        self.add_dependency(dependent, DependencyId::Struct(dependency));
    }

    pub fn add_global_dependency(&mut self, dependent: DependencyId, dependency: GlobalId) {
        self.add_dependency(dependent, DependencyId::Global(dependency));
    }

    pub fn add_function_dependency(&mut self, dependent: DependencyId, dependency: FuncId) {
        self.add_dependency(dependent, DependencyId::Function(dependency));
    }

    pub fn add_type_alias_dependency(&mut self, dependent: DependencyId, dependency: TypeAliasId) {
        self.add_dependency(dependent, DependencyId::Alias(dependency));
    }

    fn add_dependency(&mut self, dependent: DependencyId, dependency: DependencyId) {
        let dependent_index = self.get_or_insert_dependency(dependent);
        let dependency_index = self.get_or_insert_dependency(dependency);
        self.dependency_graph.update_edge(dependent_index, dependency_index, ());
    }

    fn get_or_insert_dependency(&mut self, id: DependencyId) -> PetGraphIndex {
        if let Some(index) = self.dependency_graph_indices.get(&id) {
            return *index;
        }

        let index = self.dependency_graph.add_node(id);
        self.dependency_graph_indices.insert(id, index);
        index
    }

    pub(crate) fn check_for_dependency_cycles(&self) -> Vec<(CompilationError, FileId)> {
        let strongly_connected_components = tarjan_scc(&self.dependency_graph);
        let mut errors = Vec::new();

        let mut push_error = |item: String, scc: &[_], i, location: Location| {
            let cycle = self.get_cycle_error_string(scc, i);
            let span = location.span;
            let error = ResolverError::DependencyCycle { item, cycle, span };
            errors.push((error.into(), location.file));
        };

        for scc in strongly_connected_components {
            if scc.len() > 1 {
                // If a SCC contains a type, type alias, or global, it must be the only element in the SCC
                for (i, index) in scc.iter().enumerate() {
                    match self.dependency_graph[*index] {
                        DependencyId::Struct(struct_id) => {
                            let struct_type = self.get_struct(struct_id);
                            let struct_type = struct_type.borrow();
                            push_error(struct_type.name.to_string(), &scc, i, struct_type.location);
                            break;
                        }
                        DependencyId::Global(global_id) => {
                            let global = self.get_global(global_id);
                            let name = global.ident.to_string();
                            push_error(name, &scc, i, global.location);
                            break;
                        }
                        DependencyId::Alias(alias_id) => {
                            let alias = self.get_type_alias(alias_id);
                            // If type aliases form a cycle, we have to manually break the cycle
                            // here to prevent infinite recursion in the type checker.
                            alias.borrow_mut().typ = Type::Error;

                            // push_error will borrow the alias so we have to drop the mutable borrow
                            let alias = alias.borrow();
                            push_error(alias.name.to_string(), &scc, i, alias.location);
                            break;
                        }
                        // Mutually recursive functions are allowed
                        DependencyId::Function(_) => (),
                    }
                }
            }
        }

        errors
    }

    /// Build up a string starting from the given item containing each item in the dependency
    /// cycle. The final result will resemble `foo -> bar -> baz -> foo`, always going back to the
    /// element at the given start index.
    fn get_cycle_error_string(&self, scc: &[PetGraphIndex], start_index: usize) -> String {
        let index_to_string = |index: PetGraphIndex| match self.dependency_graph[index] {
            DependencyId::Struct(id) => Cow::Owned(self.get_struct(id).borrow().name.to_string()),
            DependencyId::Function(id) => Cow::Borrowed(self.function_name(&id)),
            DependencyId::Alias(id) => {
                Cow::Owned(self.get_type_alias(id).borrow().name.to_string())
            }
            DependencyId::Global(id) => {
                Cow::Borrowed(self.get_global(id).ident.0.contents.as_ref())
            }
        };

        let mut cycle = index_to_string(scc[start_index]).to_string();

        // Reversing the dependencies here matches the order users would expect for the error message
        for i in (0..scc.len()).rev() {
            cycle += " -> ";
            cycle += &index_to_string(scc[(start_index + i) % scc.len()]);
        }

        cycle
    }
}

impl Methods {
    /// Get a single, unambiguous reference to a name if one exists.
    /// If not, there may be multiple methods of the same name for a given
    /// type or there may be no methods at all.
    fn get_unambiguous(&self) -> Option<FuncId> {
        if self.direct.len() == 1 {
            Some(self.direct[0])
        } else if self.direct.is_empty() && self.trait_impl_methods.len() == 1 {
            Some(self.trait_impl_methods[0])
        } else {
            None
        }
    }

    fn add_method(&mut self, method: FuncId, is_trait_method: bool) {
        if is_trait_method {
            self.trait_impl_methods.push(method);
        } else {
            self.direct.push(method);
        }
    }

    /// Iterate through each method, starting with the direct methods
    fn iter(&self) -> impl Iterator<Item = FuncId> + '_ {
        self.direct.iter().copied().chain(self.trait_impl_methods.iter().copied())
    }

    /// Select the 1 matching method with an object type matching `typ`
    fn find_matching_method(&self, typ: &Type, interner: &NodeInterner) -> Option<FuncId> {
        // When adding methods we always check they do not overlap, so there should be
        // at most 1 matching method in this list.
        for method in self.iter() {
            match interner.function_meta(&method).typ.instantiate(interner).0 {
                Type::Function(args, _, _) => {
                    if let Some(object) = args.first() {
                        let mut bindings = TypeBindings::new();

                        if object.try_unify(typ, &mut bindings, &interner.arith_constraints).is_ok() {
                            Type::apply_type_bindings(bindings);
                            return Some(method);
                        }
                    }
                }
                Type::Error => (),
                other => unreachable!("Expected function type, found {other}"),
            }
        }
        None
    }
}

/// These are the primitive type variants that we support adding methods to
#[derive(Copy, Clone, Hash, PartialEq, Eq, Debug)]
enum TypeMethodKey {
    /// Fields and integers share methods for ease of use. These methods may still
    /// accept only fields or integers, it is just that their names may not clash.
    FieldOrInt,
    Array,
    Slice,
    Bool,
    String,
    FmtString,
    Unit,
    Tuple,
    Function,
    Generic,
    Code,
}

fn get_type_method_key(typ: &Type) -> Option<TypeMethodKey> {
    use TypeMethodKey::*;
    let typ = typ.follow_bindings();
    match &typ {
        Type::FieldElement => Some(FieldOrInt),
        Type::Array(_, _) => Some(Array),
        Type::Slice(_) => Some(Slice),
        Type::Integer(_, _) => Some(FieldOrInt),
        Type::TypeVariable(_, TypeVariableKind::IntegerOrField) => Some(FieldOrInt),
        Type::TypeVariable(_, TypeVariableKind::Integer) => Some(FieldOrInt),
        Type::Bool => Some(Bool),
        Type::String(_) => Some(String),
        Type::FmtString(_, _) => Some(FmtString),
        Type::Unit => Some(Unit),
        Type::Tuple(_) => Some(Tuple),
        Type::Function(_, _, _) => Some(Function),
        Type::NamedGeneric(_, _) => Some(Generic),
        Type::Code => Some(Code),
        Type::MutableReference(element) => get_type_method_key(element),
        Type::Alias(alias, _) => get_type_method_key(&alias.borrow().typ),

        // We do not support adding methods to these types
        Type::TypeVariable(_, _)
        | Type::Forall(_, _)
        | Type::GenericArith(..)
        | Type::Constant(_)
        | Type::Error
        | Type::Struct(_, _)
        | Type::TraitAsType(..) => None,
    }
}

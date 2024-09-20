use fxhash::{FxHashMap as HashMap, FxHashSet as HashSet};

use crate::ssa::{
    ir::{
        basic_block::BasicBlockId,
        function::Function,
        instruction::{Instruction, InstructionId, TerminatorInstruction},
        types::Type,
        value::ValueId,
    },
    ssa_gen::Ssa,
};

impl Ssa {
    /// This pass removes `inc_rc` and `dec_rc` instructions
    /// as long as there are no `array_set` instructions to an array
    /// of the same type in between.
    ///
    /// Note that this pass is very conservative since the array_set
    /// instruction does not need to be to the same array. This is because
    /// the given array may alias another array (e.g. function parameters or
    /// a `load`ed array from a reference).
    #[tracing::instrument(level = "trace", skip(self))]
    pub(crate) fn remove_paired_rc(mut self) -> Ssa {
        for function in self.functions.values_mut() {
            remove_paired_rc(function);
        }
        self
    }
}

struct Context<'f> {
    function: &'f Function,

    last_block: BasicBlockId,
    // All inc_rc instructions encountered without a corresponding dec_rc.
    // These are only searched for in the first and exit block of a function.
    //
    // The type of the array being operated on is recorded.
    // If an array_set to that array type is encountered, that is also recorded.
    inc_rcs: HashMap<Type, Vec<IncRc>>,
}

impl<'f> Context<'f> {
    fn new(function: &'f Function) -> Self {
        let last_block = Self::find_last_block(function);
        // let all_block_params =
        Context { function, last_block, inc_rcs: HashMap::default() }
    }
}

#[derive(Clone, Debug)]
struct IncRc {
    id: InstructionId,
    array: ValueId,
    possibly_mutated: bool,
}

/// This function is very simplistic for now. It takes advantage of the fact that dec_rc
/// instructions are currently issued only at the end of a function for parameters and will
/// only check the first and last block for inc & dec rc instructions to be removed. The rest
/// of the function is still checked for array_set instructions.
///
/// This restriction lets this function largely ignore merging intermediate results from other
/// blocks and handling loops.
fn remove_paired_rc(function: &mut Function) {
    // `dec_rc` is only issued for parameters currently so we can speed things
    // up a bit by skipping any functions without them.
    if !contains_array_parameter(function) {
        return;
    }

    let mut context = Context::new(function);

    context.find_rcs_in_entry_and_exit_block();
    context.scan_for_array_sets();
    let to_remove = context.find_rcs_to_remove();
    remove_instructions(to_remove, function);
}

fn contains_array_parameter(function: &mut Function) -> bool {
    let mut parameters = function.parameters().iter();
    parameters.any(|parameter| function.dfg.type_of_value(*parameter).contains_an_array())
}

impl<'f> Context<'f> {
    fn find_rcs_in_entry_and_exit_block(&mut self) {
        let entry = self.function.entry_block();
        self.find_rcs_in_block(entry);
        self.find_rcs_in_block(self.last_block);
    }

    fn find_rcs_in_block(&mut self, block_id: BasicBlockId) {
        for instruction in self.function.dfg[block_id].instructions() {
            if let Instruction::IncrementRc { value } = &self.function.dfg[*instruction] {
                let typ = self.function.dfg.type_of_value(*value);

                // We assume arrays aren't mutated until we find an array_set
                let inc_rc = IncRc { id: *instruction, array: *value, possibly_mutated: false };
                self.inc_rcs.entry(typ).or_default().push(inc_rc);
            }
        }
    }

    /// Find each array_set instruction in the function and mark any arrays used
    /// by the inc_rc instructions as possibly mutated if they're the same type.
    fn scan_for_array_sets(&mut self) {
        // Block parameters could be passed to from function parameters.
        // Thus, any inc rcs from block parameters with matching array sets need to marked possibly mutated.
        let mut per_func_block_params: HashSet<ValueId> = HashSet::default();

        for block in self.function.reachable_blocks() {
            let block_params = self.function.dfg.block_parameters(block);
            per_func_block_params.extend(block_params.iter());
        }

        for block in self.function.reachable_blocks() {
            for instruction in self.function.dfg[block].instructions() {
                if let Instruction::ArraySet { array, .. } = self.function.dfg[*instruction] {
                    let typ = self.function.dfg.type_of_value(array);
                    if let Some(inc_rcs) = self.inc_rcs.get_mut(&typ) {
                        for inc_rc in inc_rcs {
                            if inc_rc.array == array
                                || self.function.parameters().contains(&inc_rc.array)
                                || per_func_block_params.contains(&inc_rc.array)
                            {
                                inc_rc.possibly_mutated = true;
                            }
                        }
                    }
                }
            }
        }
    }

    /// Find each dec_rc instruction and if the most recent inc_rc instruction for the same value
    /// is not possibly mutated, then we can remove them both. Returns each such pair.
    fn find_rcs_to_remove(&mut self) -> HashSet<InstructionId> {
        let mut to_remove = HashSet::default();

        for instruction in self.function.dfg[self.last_block].instructions() {
            if let Instruction::DecrementRc { value } = &self.function.dfg[*instruction] {
                if let Some(inc_rc) = self.pop_rc_for(*value) {
                    if !inc_rc.possibly_mutated {
                        to_remove.insert(inc_rc.id);
                        to_remove.insert(*instruction);
                    }
                }
            }
        }

        to_remove
    }

    /// Finds the block of the function with the Return instruction
    fn find_last_block(function: &Function) -> BasicBlockId {
        for block in function.reachable_blocks() {
            if matches!(
                function.dfg[block].terminator(),
                Some(TerminatorInstruction::Return { .. })
            ) {
                return block;
            }
        }

        unreachable!("SSA Function {} has no reachable return instruction!", function.id())
    }

    /// Finds and pops the IncRc for the given array value if possible.
    fn pop_rc_for(&mut self, value: ValueId) -> Option<IncRc> {
        let typ = self.function.dfg.type_of_value(value);

        let rcs = self.inc_rcs.get_mut(&typ)?;
        let position = rcs.iter().position(|inc_rc| inc_rc.array == value)?;

        Some(rcs.remove(position))
    }
}

fn remove_instructions(to_remove: HashSet<InstructionId>, function: &mut Function) {
    if !to_remove.is_empty() {
        for block in function.reachable_blocks() {
            function.dfg[block]
                .instructions_mut()
                .retain(|instruction| !to_remove.contains(instruction));
        }
    }
}

#[cfg(test)]
mod test {
    use std::sync::Arc;

    use crate::ssa::{
        function_builder::FunctionBuilder,
        ir::{
            basic_block::BasicBlockId, dfg::DataFlowGraph, function::RuntimeType,
            instruction::Instruction, map::Id, types::Type,
        },
    };

    fn count_inc_rcs(block: BasicBlockId, dfg: &DataFlowGraph) -> usize {
        dfg[block]
            .instructions()
            .iter()
            .filter(|instruction_id| {
                matches!(dfg[**instruction_id], Instruction::IncrementRc { .. })
            })
            .count()
    }

    fn count_dec_rcs(block: BasicBlockId, dfg: &DataFlowGraph) -> usize {
        dfg[block]
            .instructions()
            .iter()
            .filter(|instruction_id| {
                matches!(dfg[**instruction_id], Instruction::DecrementRc { .. })
            })
            .count()
    }

    #[test]
    fn single_block_fn_return_array() {
        // This is the output for the program with a function:
        // unconstrained fn foo(x: [Field; 2]) -> [[Field; 2]; 1] {
        //     [array]
        // }
        //
        // fn foo {
        //   b0(v0: [Field; 2]):
        //     inc_rc v0
        //     inc_rc v0
        //     dec_rc v0
        //     return [v0]
        // }
        let main_id = Id::test_new(0);
        let mut builder = FunctionBuilder::new("foo".into(), main_id);
        builder.set_runtime(RuntimeType::Brillig);

        let inner_array_type = Type::Array(Arc::new(vec![Type::field()]), 2);
        let v0 = builder.add_parameter(inner_array_type.clone());

        builder.insert_inc_rc(v0);
        builder.insert_inc_rc(v0);
        builder.insert_dec_rc(v0);

        let outer_array_type = Type::Array(Arc::new(vec![inner_array_type]), 1);
        let array = builder.array_constant(vec![v0].into(), outer_array_type);
        builder.terminate_with_return(vec![array]);

        let ssa = builder.finish().remove_paired_rc();
        let main = ssa.main();
        let entry = main.entry_block();

        assert_eq!(count_inc_rcs(entry, &main.dfg), 1);
        assert_eq!(count_dec_rcs(entry, &main.dfg), 0);
    }

    #[test]
    fn single_block_mutation() {
        // fn mutator(mut array: [Field; 2]) {
        //     array[0] = 5;
        // }
        //
        // fn mutator {
        //   b0(v0: [Field; 2]):
        //     v1 = allocate
        //     store v0 at v1
        //     inc_rc v0
        //     v2 = load v1
        //     v7 = array_set v2, index u64 0, value Field 5
        //     store v7 at v1
        //     dec_rc v0
        //     return
        // }
        let main_id = Id::test_new(0);
        let mut builder = FunctionBuilder::new("mutator".into(), main_id);

        let array_type = Type::Array(Arc::new(vec![Type::field()]), 2);
        let v0 = builder.add_parameter(array_type.clone());

        let v1 = builder.insert_allocate(array_type.clone());
        builder.insert_store(v1, v0);
        builder.insert_inc_rc(v0);
        let v2 = builder.insert_load(v1, array_type);

        let zero = builder.numeric_constant(0u128, Type::unsigned(64));
        let five = builder.field_constant(5u128);
        let v7 = builder.insert_array_set(v2, zero, five);

        builder.insert_store(v1, v7);
        builder.insert_dec_rc(v0);
        builder.terminate_with_return(vec![]);

        let ssa = builder.finish().remove_paired_rc();
        println!("{}", ssa);
        let main = ssa.main();
        let entry = main.entry_block();

        // No changes, the array is possibly mutated
        assert_eq!(count_inc_rcs(entry, &main.dfg), 1);
        assert_eq!(count_dec_rcs(entry, &main.dfg), 1);
    }

    // Similar to single_block_mutation but for a function which
    // uses a mutable reference parameter.
    #[test]
    fn single_block_mutation_through_reference() {
        // fn mutator2(array: &mut [Field; 2]) {
        //     array[0] = 5;
        // }
        //
        // fn mutator2 {
        //   b0(v0: &mut [Field; 2]):
        //     v1 = load v0
        //     inc_rc v1
        //     store v1 at v0
        //     v2 = load v0
        //     v7 = array_set v2, index u64 0, value Field 5
        //     store v7 at v0
        //     v8 = load v0
        //     dec_rc v8
        //     store v8 at v0
        //     return
        // }
        let main_id = Id::test_new(0);
        let mut builder = FunctionBuilder::new("mutator2".into(), main_id);

        let array_type = Type::Array(Arc::new(vec![Type::field()]), 2);
        let reference_type = Type::Reference(Arc::new(array_type.clone()));

        let v0 = builder.add_parameter(reference_type);

        let v1 = builder.insert_load(v0, array_type.clone());
        builder.insert_inc_rc(v1);
        builder.insert_store(v0, v1);

        let v2 = builder.insert_load(v1, array_type.clone());
        let zero = builder.numeric_constant(0u128, Type::unsigned(64));
        let five = builder.field_constant(5u128);
        let v7 = builder.insert_array_set(v2, zero, five);

        builder.insert_store(v0, v7);
        let v8 = builder.insert_load(v0, array_type);
        builder.insert_dec_rc(v8);
        builder.insert_store(v0, v8);
        builder.terminate_with_return(vec![]);

        let ssa = builder.finish().remove_paired_rc();
        let main = ssa.main();
        let entry = main.entry_block();

        // No changes, the array is possibly mutated
        assert_eq!(count_inc_rcs(entry, &main.dfg), 1);
        assert_eq!(count_dec_rcs(entry, &main.dfg), 1);
    }

    #[test]
    fn separate_entry_and_exit_block_fn_return_array() {
        // brillig fn foo f0 {
        //     b0(v0: [Field; 2]):
        //       jmp b1(v0)
        //     b1():
        //       inc_rc v0
        //       inc_rc v0
        //       dec_rc v0
        //       return [v0]
        //   }
        let main_id = Id::test_new(0);
        let mut builder = FunctionBuilder::new("foo".into(), main_id);
        builder.set_runtime(RuntimeType::Brillig);

        let inner_array_type = Type::Array(Arc::new(vec![Type::field()]), 2);
        let v0 = builder.add_parameter(inner_array_type.clone());

        let b1 = builder.insert_block();
        builder.terminate_with_jmp(b1, vec![v0]);

        builder.switch_to_block(b1);
        builder.insert_inc_rc(v0);
        builder.insert_inc_rc(v0);
        builder.insert_dec_rc(v0);

        let outer_array_type = Type::Array(Arc::new(vec![inner_array_type]), 1);
        let array = builder.array_constant(vec![v0].into(), outer_array_type);
        builder.terminate_with_return(vec![array]);

        // Expected result:
        //
        // brillig fn foo f0 {
        //     b0(v0: [Field; 2]):
        //       jmp b1(v0)
        //     b1():
        //       inc_rc v0
        //       return [v0]
        //   }
        let ssa = builder.finish().remove_paired_rc();
        let main = ssa.main();

        assert_eq!(count_inc_rcs(b1, &main.dfg), 1);
        assert_eq!(count_dec_rcs(b1, &main.dfg), 0);
    }

    #[test]
    fn exit_block_single_mutation() {
        // fn mutator(mut array: [Field; 2]) {
        //     array[0] = 5;
        // }
        //
        // acir(inline) fn mutator f0 {
        //     b0(v0: [Field; 2]):
        //       jmp b1(v0)
        //     b1(v1: [Field; 2]):
        //       v2 = allocate
        //       store v1 at v2
        //       inc_rc v1
        //       v3 = load v2
        //       v6 = array_set v3, index u64 0, value Field 5
        //       store v6 at v2
        //       dec_rc v1
        //       return
        //   }
        let main_id = Id::test_new(0);
        let mut builder = FunctionBuilder::new("mutator".into(), main_id);

        let array_type = Type::Array(Arc::new(vec![Type::field()]), 2);
        let v0 = builder.add_parameter(array_type.clone());

        let b1 = builder.insert_block();
        builder.terminate_with_jmp(b1, vec![v0]);

        builder.switch_to_block(b1);
        // We want to make sure we go through the block parameter
        let v1 = builder.add_block_parameter(b1, array_type.clone());

        let v2 = builder.insert_allocate(array_type.clone());
        builder.insert_store(v2, v1);
        builder.insert_inc_rc(v1);
        let v3 = builder.insert_load(v2, array_type);

        let zero = builder.numeric_constant(0u128, Type::unsigned(64));
        let five = builder.field_constant(5u128);
        let v8 = builder.insert_array_set(v3, zero, five);

        builder.insert_store(v2, v8);
        builder.insert_dec_rc(v1);
        builder.terminate_with_return(vec![]);

        let ssa = builder.finish().remove_paired_rc();
        let main = ssa.main();

        // No changes, the array is possibly mutated
        assert_eq!(count_inc_rcs(b1, &main.dfg), 1);
        assert_eq!(count_dec_rcs(b1, &main.dfg), 1);
    }

    #[test]
    fn exit_block_mutation_through_reference() {
        // fn mutator2(array: &mut [Field; 2]) {
        //     array[0] = 5;
        // }
        // acir(inline) fn mutator2 f0 {
        //     b0(v0: &mut [Field; 2]):
        //       jmp b1(v0)
        //     b1(v1: &mut [Field; 2]):
        //       v2 = load v1
        //       inc_rc v1
        //       store v2 at v1
        //       v3 = load v2
        //       v6 = array_set v3, index u64 0, value Field 5
        //       store v6 at v1
        //       v7 = load v1
        //       dec_rc v7
        //       store v7 at v1
        //       return
        //   }
        let main_id = Id::test_new(0);
        let mut builder = FunctionBuilder::new("mutator2".into(), main_id);

        let array_type = Type::Array(Arc::new(vec![Type::field()]), 2);
        let reference_type = Type::Reference(Arc::new(array_type.clone()));

        let v0 = builder.add_parameter(reference_type.clone());

        let b1 = builder.insert_block();
        builder.terminate_with_jmp(b1, vec![v0]);

        builder.switch_to_block(b1);
        let v1 = builder.add_block_parameter(b1, reference_type);

        let v2 = builder.insert_load(v1, array_type.clone());
        builder.insert_inc_rc(v1);
        builder.insert_store(v1, v2);

        let v3 = builder.insert_load(v2, array_type.clone());
        let zero = builder.numeric_constant(0u128, Type::unsigned(64));
        let five = builder.field_constant(5u128);
        let v6 = builder.insert_array_set(v3, zero, five);

        builder.insert_store(v1, v6);
        let v7 = builder.insert_load(v1, array_type);
        builder.insert_dec_rc(v7);
        builder.insert_store(v1, v7);
        builder.terminate_with_return(vec![]);

        let ssa = builder.finish().remove_paired_rc();
        let main = ssa.main();

        // No changes, the array is possibly mutated
        assert_eq!(count_inc_rcs(b1, &main.dfg), 1);
        assert_eq!(count_dec_rcs(b1, &main.dfg), 1);
    }
}

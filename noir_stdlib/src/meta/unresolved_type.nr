impl UnresolvedType {
    #[builtin(unresolved_type_is_field)]
    // docs:start:is_field
    comptime fn is_field(self) -> bool {}
    // docs:end:is_field
}

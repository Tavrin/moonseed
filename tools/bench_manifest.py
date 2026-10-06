"""Revision 2: additive sources; explicit scaling substitutions for profiling only."""
REVISION = 2
# Source loop bounds (not arbitrary numeric literals). All replacements recorded.
BOUNDS = {
    'empty': [], 'fib': ['fib(34)'], 'table_fields': ['600000'],
    'alloc_churn': ['3000000'], 'strings': ['360000', '3000'], 'sort': ['100000'],
    'numeric_loops': ['60000000', '10000000', '2000'], 'generic_for': ['40000'],
    'method_calls': ['4000000'], 'closures': ['10000000'], 'coroutines': ['3000000'],
    'metamethods': ['4000000'], 'patterns': ['200000'], 'native_calls': ['2000000'],
    'application': ['12000'], 'branches': ['16000000'], 'array_access': ['15000000'],
    'globals': ['14000000'], 'tail_recursion': ['10000000'], 'field_writes': ['18000000'],
    'string_concat': ['1400000'], 'string_format': ['1000000'],
}
TAGS = {
    'empty': ['startup'], 'fib': ['core VM'], 'table_fields': ['tables'],
    'alloc_churn': ['allocation/GC', 'tables'], 'strings': ['strings', 'allocation/GC'],
    'sort': ['tables', 'stdlib'], 'numeric_loops': ['core VM'],
    'generic_for': ['tables', 'core VM'], 'method_calls': ['core VM', 'tables'],
    'closures': ['core VM'], 'coroutines': ['core VM'], 'metamethods': ['core VM', 'tables'],
    'patterns': ['strings', 'stdlib'], 'native_calls': ['stdlib'],
    'application': ['core VM', 'tables', 'strings'], 'branches': ['core VM'],
    'array_access': ['tables'], 'globals': ['tables', 'core VM'],
    'tail_recursion': ['core VM'], 'field_writes': ['tables'],
    'string_concat': ['strings', 'allocation/GC'], 'string_format': ['strings', 'stdlib'],
}

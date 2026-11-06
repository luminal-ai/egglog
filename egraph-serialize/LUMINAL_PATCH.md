Vendored from the crates.io egraph-serialize 0.3.0 package (MIT).

Node and class IndexMap lookup uses Fx hashing. Insertion order, IDs, values,
serialization format, and algorithms are unchanged. This targets the measured
SipHash overhead in large immutable compiler snapshots. Upstream sources and
JSON round-trip fixtures are retained; generated SVG visualizations are omitted.

The `compact` module is the compiler's numeric snapshot format. Dense node/class
IDs encode legacy lexical tie order, while contiguous maps retain database row
order and direct ID lookup. Text IDs remain available for synthetic values and
fixtures. Constructor/sort names are shared, empty annotations occupy one word,
and child operands and class memberships use shared contiguous arenas.

Rows, indexes and metadata use graph-scoped copy-on-write storage; lazy renderers
can retain a snapshot without copying every node. Class and operator indexes are
built once. Edited operand slices copy only the affected node. JSON stores dense
IDs as numbers, text IDs as strings, and maps as ordered row arrays. In-memory
mutation proofs and derived indexes are never persisted. The legacy module and
its diagnostic JSON format remain available independently.

The backend's `compact_snapshot` tests compare exact representatives, metadata,
lexical ordering (including ambiguous sort-name prefixes), seeded producer
choices, copy-on-write isolation, archive reloads, and mutation/fork rejection.

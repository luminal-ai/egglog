Vendored from luminal-ai/egglog commit 1bb30831e11121bd639caf93d84465d8d110e3f7.

The SymbolGen fresh-name encoding now length-prefixes the hint and separates its
counter. Previously the second temporary for `v1` collided with the first for
`v11`. An imported 512-stage add/multiply chain lost 50 stages before saturation.
The backend deep-arithmetic regression and symbol_collision_tests cover this.
The original MIT license and source tests are retained.

ExecutionState mutation buffers use a sparse, table-id-ordered map. Large
captured graphs introduce many global binding tables, while each action writes
only a few. This avoids allocating and dropping a dense table-id-sized array
for every action, retaining buffer flush order and mutation notifications.

The optional `egglog::query` trace target brackets individual query execution
with its rule description and elapsed time. Logging is off by default; the
host-only `pt2_saturation` tool enables it to attribute stalled compiler joins.
`EGGLOG_PROFILE_QUERY_TASKS=1` also sums elapsed time inside forked join and
action tasks by rule. Parent query timers otherwise omit that work. This is a
diagnostic sum of concurrent task durations, not elapsed compilation time.

Serialization interns canonical class IDs for one immutable snapshot, reuses
primitive nodes, and builds class metadata once. Container edge rotation is
preserved. `refresh_serialized_subsumption` validates all existing function
rows and visible global bindings before updating subsumption flags in place.
It permits only explicitly named new annotation relations to be omitted and
fails closed on structural changes. Snapshot node IDs stay stable even when
subsumption moves the live table's offsets. The backend's `compile_snapshot`
regressions cover unions, insertion, deletion, annotations and global metadata.

Consecutive proof-free check commands share one read-only backend query round;
mutating commands remain strict execution boundaries. Failures are returned in
source order and temporary rules/external callbacks are released on failure.
Rule/check shadowing uses a borrowed global table and a local name map, retaining
global alias and duplicate-local rejection without cloning every global name.
The backend's batched_checks tests cover success/failure order, command boundaries,
state preservation, scope isolation and the upstream shadowing fixtures.

Free-join probes borrow a column index from a cache scoped to a single rule-set
execution. Only tables referenced by its plans receive entries. The cache drops
before merging writes so index reset retains unique ownership; the core relation
suite exercises serial/parallel joins, updates, rebuilds and query intersections.

Rule-set construction also shares immutable timestamp/literal constraint subsets
across seminaive variants. Its cache exists only while the builder holds the
database; a new builder refreshes it after execution/rebuild. Plan headers share
these subsets and diagnostic symbol maps use Arc ownership. The cached-constraint
regression covers repeated atoms, sparse/empty subsets, deletion, insertion and
report generation; the core relation and bridge suites pass.

Dynamic column probes identify the cached index by column ID and borrow it
through the probe's owned trie node. Index storage uses OnceLock values rather
than per-probe Arc clones, removing shared atomic reference-count contention.
Execution-only program loading no longer retains a duplicate resolved AST;
resolve-only calls and proof history keep their original command inventories.

Constructor ingestion now interprets total constructor trees and fresh internal
let initialization without building a temporary join query for every fact.
The fast path admits only literals, constructors and initialized internal let
reads, retains a flush per action, and excludes term encoding/proof execution.
A regression compares canonical retained rows against compiled evaluation.
Incremental rebuild also skips reverse-index refresh on an empty representative
delta; the index's version tracker retains intervening row changes until needed.

Ground constructor facts resolve their sorts directly from declared constructor
signatures. Primitive calls, unknown names, invalid inputs and general constraints
retain the normal type solver. Constructor-action ingestion uses a checked flush
so illegal merge failures are reported at the same action boundary. Differential
type-resolution/action tests compare the shortcut against the general evaluator.

Serialization retains singleton class representatives inline and advances a
cursor for multi-node edge rotation, preserving the exact rotate-before-read
sequence. Sort-cache lookups borrow names once, and ordered output maps reserve
space for known rows before insertion. A differential representative-sequence
test covers singleton and multi-node classes; snapshot tests cover projection,
subsumption, global bindings and structural invalidation.

## External snapshot ownership

The compact format, exporter, refresh policy and associated tests are owned by
`luminal-ai/luminal_trainium` in `crates/trainium-snapshot`. Egglog retains a
read-only `SnapshotReader` for canonical values, function rows, schema costs,
container representations and database/table versions. This interface does
not depend on any compiler snapshot format. Egglog continues to depend on the
unmodified crates.io `egraph-serialize` 0.3.0 diagnostic format.

These patches were extracted from Trainium commit
`f39babf9a2f7f2998b5226fbc5745c117cef5f77` on top of
`1bb30831e11121bd639caf93d84465d8d110e3f7`, including Luminal's subsumed-insert
support. Upstream files omitted from the Trainium vendor tree remain intact.

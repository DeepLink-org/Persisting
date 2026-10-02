# Design principles

Preserve input fidelity through documented codecs. Keep one normalized
Storyline model and one owner for storage and query semantics. Pin Source
versions for each operation, retain provenance for transformed output, and
bound parsing, I/O, and query work. Failed publication must leave the previous
committed data readable. Keep datasets portable across files, object storage,
CLI, and Web consumers.

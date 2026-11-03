# Path identity and mount aliases (target schema v7)

The filename records the original v6 design milestone. Growth history now has
its own v6 incarnation-safe schema; the path-instance graph described here is
therefore the next schema version (v7).

## Decision

Do not mark a scan exact when the same directory inode is reachable through
multiple mount paths. Schema v6 attaches every child edge to `(parent_dev,
parent_inode)`, so two path instances of one directory collapse into one edge
set. The Rocky 9 qualification guest reproduced this at `/boot/efi` and `/efi`:
both expose `/dev/vda2`, and the scanner correctly pruned one path and left the
index dirty. Suppressing that error would hide valid paths.

Schema v7 must represent **inode identity** and **path-instance identity** as
separate things. A path instance is one occurrence of a directory in the
visible mount tree; an inode remains the filesystem object whose allocated
blocks must not be counted twice.

## Data model

Keep `inodes` as one row per `(device, inode, incarnation)` for metadata and
allocated blocks. `birth_ns` is part of that identity wherever a filesystem
inode number is used to join or attribute allocation. A long scan can observe
an inode, then observe a deleted-and-reused inode number at another path; the
same `(device, inode)` with different birth times is a scan-time identity
conflict, not a hardlink or mount alias, and must keep coverage dirty. Add a
path graph whose identity belongs to an occurrence:

`birth_ns=0` means the filesystem/kernel could not provide birth time. When
all repeated observations of an inode number have zero birth time, this field
cannot distinguish a hardlink from inode-number reuse during the scan. Such a
case needs another generation source or an explicit uncertainty check before
the staged graph can claim exact identity; zero must never be treated as a
verified incarnation value. The scanner's conservative policy is to mark any
repeated `(device,inode)` group with only zero birth times as coverage-incomplete,
including possible hardlinks. A later identity source may safely relax this
only after it can prove the observations refer to one incarnation.

```sql
CREATE TABLE paths (
    path_id INTEGER PRIMARY KEY,
    parent_path_id INTEGER NOT NULL,
    name BLOB NOT NULL,
    dev_id INTEGER NOT NULL,
    inode INTEGER NOT NULL,
    birth_ns INTEGER NOT NULL DEFAULT 0,
    kind TEXT NOT NULL,
    mount_id INTEGER NOT NULL DEFAULT -1,
    prime INTEGER NOT NULL,
    cycle INTEGER NOT NULL DEFAULT 0,
    CHECK (path_id > 0),
    CHECK (parent_path_id >= -1),
    CHECK (prime IN (0,1)),
    CHECK (cycle IN (0,1)),
    UNIQUE(parent_path_id, name)
);
CREATE UNIQUE INDEX paths_single_root ON paths(parent_path_id)
    WHERE parent_path_id=-1;
CREATE INDEX paths_target ON paths(dev_id, inode, birth_ns);
CREATE INDEX paths_parent ON paths(parent_path_id);
CREATE INDEX paths_inode_prime ON paths(dev_id, inode, birth_ns, prime);
CREATE UNIQUE INDEX paths_one_prime ON paths(dev_id, inode, birth_ns) WHERE prime=1;

CREATE TABLE dir_totals (
    path_id INTEGER PRIMARY KEY REFERENCES paths(path_id) ON DELETE CASCADE,
    visible_bytes INTEGER NOT NULL,
    visible_entries INTEGER NOT NULL,
    unique_bytes INTEGER NOT NULL,
    unique_inodes INTEGER NOT NULL
);
```

The root is a self-describing `paths` row with `parent_path_id = -1` and its
absolute root name. Every directory occurrence has its own `path_id`, and children refer to
that ID. Files and directories both point to the separate `inodes` table.
Directory path rows may share `(dev_id,inode,birth_ns,mount_id)` while retaining
distinct parent/name chains. Keep inode allocation attribution separate from
path display: hard links and mount aliases can create many paths for one inode
incarnation.

Define both directory totals explicitly. `visible_*` reports the contents
reachable through that path instance and may overlap another instance. The
`visible_bytes` sums allocated blocks for each path occurrence and
`visible_entries` counts path rows, including the directory itself; hardlinks
and aliases can therefore overlap. `unique_*` follows the graph's single prime
path attribution: an inode contributes only beneath its prime path, so a
subdirectory can have zero unique bytes for a visible inode whose prime path is
elsewhere. The global root `unique_*` total counts each inode once. A treemap
rooted at a particular alias uses visible allocation and labels overlapping
mount aliases; filesystem-wide usage uses unique allocation. Do not sum
overlapping visible directory rows and call the result physical usage.

## Scanner and publication

Replace the global `(dev,inode) -> first path` prune with path-instance tasks.
Each bounded walk task carries its parent `path_id` and an ancestor identity
guard. Enumerating a directory creates one path row for each visible child. A
repeated inode that is not an ancestor is an alias and is traversed as another
path instance. An identity already in the current ancestor chain is a real
traversal cycle: record the exact path and keep coverage uncertain for that
scope.

Use a bounded producer/SQLite staging-writer queue, as today. Do not retain one
full `PathBuf` per file or an unbounded alias map in memory. Assign each
directory task a `path_id` and pass it to child work; derive absolute paths by
walking parent IDs. Record mount ID (statx `mnt_id` where available) alongside
device/inode so mount topology and same-filesystem aliases remain
distinguishable. Do not hide directory state in a full-path map: retaining one
`PathBuf` per file at 10m scale would add a large, unmeasured memory cost.

The pinned `jwalk 0.8.1` source originally cloned one `ReadDirState` from a
parent read into all sibling directory tasks and did not pass a child entry's
`DirEntryState` into that child's read callback. Dux now carries a local,
backward-compatible fork in `vendor/jwalk`: the new
`ClientState::read_dir_state_for_entry` hook derives each child task's state
from that entry, while its default preserves the old behavior. Existing Dux
scans now use this hook to carry ephemeral parent path IDs into each directory
read task. Accepted nodes stage `path_id`, `parent_path_id`, and the available
statx mount ID beside their v6 inode edge; birth time and mount ID come from one
identity-checked statx request. Non-ancestor aliases are now traversed into the
disk-backed staging graph. Shared ancestor chains stop only cycle-closing paths;
the cycle row is retained with a marker and is not recursed. Repeated directory
identities are counted from SQLite staging rather than kept in an in-memory
alias map. The legacy v6 projection chooses one row per `(parent_dev,parent_inode,
name)` edge and omits cycle-closing edges, while marking coverage incomplete
whenever path aliases exist. It also marks coverage incomplete if repeated scans
of an inode-parent/name edge resolve to different object identities, or if one
inode's allocated size changes between path observations. This keeps v6
structurally usable without claiming that it represents every visible alias.
Before deriving v6 totals, the scanner copies every staged occurrence into the
v7-shaped `paths` table in the private scan database, including each cycle
marker and `birth_ns`, adds the path indexes, and checks exact row/field
correspondence, root and parent identity, and exactly one prime path per inode
incarnation. It detects differing birth times for one `(dev,inode)` as a
separate coverage conflict even when the observed allocated sizes match. It
builds path-local
visible and prime-attributed unique totals bottom-up, then verifies the root
visible sum against all staged occurrences and the root unique sum against
distinct inode allocations. It reports staged path-instance and directory-alias
counts. Tests cover
sibling task state, ancestor-vs-alias detection, and the compatibility
projection; the ext4 qualification confirms two alias rows in staging while v6
retains only one visible probe path. The path graph and metrics are discarded
after the v6 projection and are not published in the live index. Production v7
publication, directory totals, event updates, queries, cycle-mount
qualification, and Rocky alias exactness remain open blockers.

Build the new schema in a private staging database, preserve compatible growth
history, replay captured events through the scan boundary, verify path counts,
inode allocation attribution, both directory-total definitions, and cycle/
coverage status, then atomically publish. Keep the v6 index readable until the
v7 candidate verifies. Never migrate by dropping the live index or by blessing
a v6 single-path view as v7-exact.

## Event semantics

Fanotify events identify filesystem objects and parent/name pairs, not always a
unique mount-path instance. Resolve an event to all matching parent path IDs
within the event's mount identity, then apply the operation to those visible
instances in one transaction. If mount identity is absent or ambiguous, mark
the smallest proven scope dirty and reconcile it; do not pick an arbitrary
alias. Rename, delete, and create operations must update path rows and
directory-instance totals without changing physical inode allocation more than
once. Directory moves must update descendant path derivation through the
path parent link, not rewrite every descendant string.

## Query and history semantics

- `find` returns every matching visible path instance, with an option to group
  aliases by inode. Path filters start from a path instance, not only an inode.
- Large-file views can show all visible paths but identify aliases of the same
  inode and avoid presenting duplicate allocation as extra physical usage.
- Directory rankings use path-instance totals; root usage and reclaimed-space
  totals use unique inode attribution.
- Growth remains keyed by inode incarnation. A path-specific growth row joins
  to the path instances visible for that inode; group-by-inode reports remain
  unique, while group-by-path reports explicitly allow alias overlap.
- Query pagination keys include the path ID, preserving stable order
  when an inode has several paths.

## Migration and verification gates

Schema v6 is not rewritten in place. Startup keeps it queryable and performs an
atomic v7 rebuild. The old binary must refuse a newer v7 schema without
mutation. Retain the v6 backup until the new candidate is published and passes
verification.

Before enabling v7 by default, add differential fixtures for: two bind mounts
of the same directory; aliases nested under different parents; a bind-mount
cycle; duplicate filesystem roots; hard links beneath aliases; rename/create/
delete through each alias; mount add/remove during a scan; permission denial on
one alias; crash before and after publication; and event overflow during
reconciliation. Assert exact visible path sets, inode allocation once globally,
visible and unique rollups, growth history, restart replay, and bounded memory
at 100k/1m/10m scale. Include the Rocky `/efi` and `/boot/efi` mount topology as
a packaged clean-install regression.

This is a release blocker until implemented and qualified; the existing
`coverage_incomplete` signal is correct and must remain enabled during rollout.

## Current packaged failure evidence

The strict R40 clean-install lifecycle gate now requires the daemon to report
`caught_up=true`, no dirty/paused state, and no scan in progress. On Rocky Linux
9.8, the exact package transitions completed but the R40 daemon failed that
gate: `/efi` and `/boot/efi` repeatedly resolved to the same directory
identity, and after three incomplete scans automatic retries paused. The
machine-readable report is
`target/readiness/production-review-20261030/core-limit-r40/package-lifecycle-r40-rpm-rocky9-readiness-gate-structured.json`.
This confirms the v7 graph is required for this mount topology; package
installation and systemd active state alone do not establish exact coverage.

# dux TUI design: answer-first, simple, and drillable

## Primary navigation

Keep only four first-class screens, mapped to both number keys and memorable
letters:

1. **Overview (1/o)** — Is the disk healthy, what is largest, what is growing,
   and how much can I reclaim?
2. **Explore (2/e)** — Browse/search the indexed tree and change distribution.
3. **Activity (3/a)** — What changed, when, how quickly, and which writer owns it?
4. **Reclaim (4/r)** — Measured cleanup candidates, evidence, safety, and action.

The first frame should answer the incident without navigation. Advanced
groupings belong in a single distribution selector, not as more permanent panels.

## Overview layout

- One-line health strip: disk used/free, inode pressure, net growth/day, ETA,
  daemon state, queue depth, lag, and index confidence.
- Three ranked cards: **Largest now**, **Growing fastest**, **Reclaimable now**.
- One distribution chart using aligned horizontal bars. Default to top-level
  location; bracket keys change distribution without changing selection.
- A persistent breadcrumb and full selected path at the bottom.
- Use color only for meaning: red=urgent, amber=needs review, cyan=selection,
  gray=normal. Magnitude is communicated by bar length, not a rainbow palette.

## Distribution/grouping catalog

### Location and storage topology

- Top-level directory and arbitrary subtree
- Mountpoint/filesystem
- Physical device, partition, LVM logical volume, ZFS dataset, or Btrfs subvolume
- Local versus network versus memory-backed filesystem
- User homes, application data, logs, caches, temporary data, OS/package data
- Depth level and directory fan-out

### Ownership and workload

- User/UID and group/GID
- Individual home directory
- System service/systemd unit
- Application profile: database, web server, observability, package manager
- Docker/Podman container
- Container image, shared layer, writable layer, log, volume, and build cache
- Kubernetes namespace, pod, workload, container, PVC, and ephemeral storage
- Process holding deleted-open space

### File characteristics

- File versus directory versus symlink versus special object
- Extension and MIME/content family
- Size bands: tiny, small, medium, large, huge
- Age/mtime bands: today, week, month, quarter, year, stale
- Sparse versus allocated size
- Hardlinked versus single-link data
- Hidden files, dot-caches, package artifacts, archives, databases, logs, media

### Change and behavior

- Growth and shrink over 5m, 1h, 24h, 7d, and custom ranges
- Current write rate and projected daily growth
- Newly created, recently modified, renamed, and deleted
- High-churn paths: many events but little net growth
- Largest positive and negative deltas
- Growth acceleration: faster than the previous window
- Stable/cold data versus hot data

### Reclaimability and safety

- **Safe**: index free pages, reproducible caches with canonical cleanup commands
- **Safe-ish**: package downloads, build caches, trash, bounded journal retention
- **Verify**: stopped containers, orphan volumes, old temp files, old kernels
- **Risky**: user/application data, database files, unknown orphan-looking paths
- Reclaimable amount versus total amount
- Cleanup confidence, evidence, owner, last use, and rollback/backup availability
- Deleted-open files requiring process restart rather than file deletion

### Capacity discrepancy and risk

- Indexed allocated bytes versus filesystem used bytes
- Deleted-open bytes
- Reserved filesystem blocks and snapshot/COW overhead
- Unindexed, inaccessible, dirty, or stale coverage
- Disk capacity pressure and inode pressure
- Queue saturation, event resolution failures, scan/reconcile progress

## Interaction rules

- Enter drills down; Backspace/Left returns; slash searches everywhere.
- Tab moves between the few visible cards; arrows never change screen.
- Every cleanup row opens an evidence drawer before showing a command.
- Destructive actions are never the default key and require an explicit typed
  confirmation; initially keep the TUI advisory-only.
- Preserve selection while live data reorders. Show a small “updated” pulse,
  never jump the cursor.
- Support narrow terminals by stacking cards and hiding secondary columns.
- Question mark opens a one-screen contextual key guide.

## Recommended implementation order

1. Overview health strip and three answer cards.
2. Unified distribution selector with location, owner, age, type, and extension.
3. Activity timeline/window selector.
4. Reclaim evidence drawer and safety filters.
5. Service/container/Kubernetes attribution when reliable metadata is available.
6. Optional true treemap only after keyboard navigation and narrow-terminal
   behavior are excellent; aligned bars should remain the accessible default.

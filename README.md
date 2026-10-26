# nanalloc
## An ultra tiny `no_std` allocator for `wasm32-unknown-unknown`

The current size target (no compression) is to be under 2KB.
*(This is measured by building a release wasm module with exported `alloc` and `dealloc` symbols)*

### Features
| Status | Feature |
| --- | --- |
| WIP | General purpose allocator (allocate large blocks of various sizes, 8192 minimum) |
| WIP | Per-class allocation pools (special structures that redistribute large blocks for small allocations). Planned support for 32, 64, 128, 256, 512, 1024 and 2048 byte allocations. |
| WIP | Pool cleanup (free blocks of empty pools when unused) |
| WIP | Pass internal tests |

### Internals
This allocator internally allocates a single wasm page for its entire bookkeeping - a large binary
tree where each node is a power of two block, combined from its two smallest children (except for leafs).

This in turn means that: 
- All blocks are powers of two (the leafs being 2^13 bytes long)
- Blocks naturally reunite into larger blocks through propagation.
- Finding blocks is O(log N), freeing is O(1)


### Limitations
- An entire wasm page (2^16 bytes or 64KB) is allocated in advance for heap bookkeeping.
- The entire heap is limited to 2^30 bytes (1GB) due to internal limitations.
- The smallest block size is 8KB, and the largest pool class is 2KB. Anything allocated beyond is rounded
to 8KB, which should be kept in mind
- Alignment is only guaranteed up to 8192 bytes. In any other case this allocator will not allocate.
- Since block sizes are powers of two, allocating just above power of two will result in a lot of waste (allocating 8193 bytes for example will result in 16384 bytes total allocated)

### Licensing
MIT or Apache 2.0, under your choice.
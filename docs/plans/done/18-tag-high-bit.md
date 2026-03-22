# 18 — Move packed dirent tag to bit 63

## Problem

The inode/link tag sits at bit 0, which forces gen to occupy [16:1]
requiring a 1-bit shift for both encoding and extraction. The link
pointer recovery needs an AND+OR (mask off bit 0 and bits [63:61],
then restore sign extension).

## Approach

Move the tag to bit 63. Since kernel pointers already have bit 63 = 1
(canonical sign extension), the tag naturally coincides with the pointer's
sign bit for links. This lets gen sit at [15:0] (no shift), simplifies
link pointer recovery to a single OR, and lets is_link use the sign bit.

## New layout

**Inode** (bit 63 = 0):

    [63]     0        tag
    [62:61]  d_type   2 bits
    [60]     in_base  1 bit
    [59:16]  ino      44 bits
    [15:0]   gen      16 bits

**Link** (bit 63 = 1):

    [63]     1        tag (= kernel sign extension bit)
    [62:61]  d_type   2 bits (borrowed from sign extension)
    [60]     in_base  1 bit  (borrowed from sign extension)
    [59:0]   pointer bits [59:0]

**Tombstone**: val == 0 (falls in inode tag space; ino=0 is invalid).

## Changes

### Predicates

- `is_link`: `p.val & 1` → `(s64)p.val < 0`
- `is_inode`: `p.val && !(p.val & 1)` → `p.val && (s64)p.val >= 0`

### Decoders

- `d_type`: `>> 62` → `>> 61`
- `in_base`: `>> 61` → `>> 60`
- `ino`: `>> 17, mask 0xFFFFFFFFFFF` → `>> 16, mask 0xFFFFFFFFFFF`
- `gen`: `(u16)(val >> 1)` → `(u16)val`
- `base`: `(val & 0x1FFFFFFFFFFFFFFE) | 0xE000000000000000`
         → `val | 0x7000000000000000`

### Encoders

- inode: d_type `<< 62` → `<< 61`, in_base `<< 61` → `<< 60`,
         ino `<< 17` → `<< 16`, gen `<< 1` → no shift
- link: d_type `<< 62` → `<< 61`, in_base `<< 61` → `<< 60`,
        ptr mask `0x1FFFFFFFFFFFFFFE` → `0x0FFFFFFFFFFFFFFF`,
        remove `| 1`, add `| (1ULL << 63)`
- link WARN: `((ptr >> 57) & 0x7F) != 0x7F` →
             `((ptr >> 60) & 0xF) != 0xF`

### Docs

- docs/staging.md: update layout, accessors, pointer recovery

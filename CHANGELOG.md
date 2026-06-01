# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://github.com/cat198x/cat198x/releases/tag/v0.1.0) - 2026-06-01

### Fixed

- *(apply)* verify + fsync the cross-device rollback copy before delete
- *(apply)* journal quarantine so it can be rolled back
- *(apply)* fsync the destination before deleting the source on move
- *(quarantine)* prevent data loss in quarantine filename and prune
- *(plan)* align generator matching with the verdict path (either hash, CRC+size)
- *(db)* match DAT ROMs by SHA1 or CRC+size; stop dropping CRC-only entries
- *(scan,db)* store full + headerless hashes; match a DAT against either
- *(dat)* capture non-self-closing <rom> and <device_ref> elements

### Other

- add flagship workspace CLAUDE.md
- record skeleton lint adaptations for the rescue
- add release-plz + cargo-dist release pipeline
- add CI workflow with fmt, clippy, coverage, and build gates
- rustfmt the tree
- adopt 198x skeleton quality config
- rebrand ROMShelf -> Cat198x in README and SPECIFICATION
- [**breaking**] rebrand ROMShelf -> Cat198x
- *(deps)* upgrade md-5/sha1/sha2 0.10 -> 0.11
- *(deps)* refresh Cargo.lock to latest compatible versions
- *(deps)* upgrade reqwest 0.12 -> 0.13
- *(deps)* upgrade rusqlite 0.31 -> 0.40
- *(deps)* replace sevenz-rust with maintained sevenz-rust2
- *(deps)* upgrade quick-xml 0.31 -> 0.40
- *(deps)* upgrade self_update 0.41 -> 0.44
- *(deps)* upgrade indicatif 0.17 -> 0.18
- *(deps)* upgrade toml 0.8 -> 1
- *(deps)* upgrade directories 5 -> 6
- *(deps)* upgrade thiserror 1 -> 2
- extract plan-execution engine into the library
- add README
- *(deps)* build zip with deflate codec only
- *(deps)* upgrade zip 0.6 -> 8.6
- resolve all clippy warnings
- *(db)* wrap DAT import and scan write-back in transactions
- import Romshelf as the Cat198x rescue baseline

zingod, bundled with Zeal as zeal-zingod

zingod 1.1.0 is Zingo Labs' Zcash indexer. Copyright 2024 ZingoLabs.
It is licensed under the Apache License, Version 2.0: zingod-LICENSE is
zingod's notice and Apache-2.0.txt the licence's full text.

Source: https://github.com/zingolabs/zingod, tag v1.1.0, commit
bfce8b2b433578054dec82928d2707e523ec75b7.
Each Zeal release that bundles zingod also has the asset
zingod-src-1.1.0.tar.zst: that source with every dependency vendored,
which Zeal built zeal-zingod from.

The .proto files compiled into zingod, in packages/zingod-proto/proto/:
- compact_formats.proto, from lightwallet-protocol, which zingod vendors in
  packages/zingod-proto/lightwallet-protocol/: MIT,
  lightwallet-protocol-LICENSE.
- service.proto, zingod-proto's modified copy of lightwallet-protocol's,
  under the Zcash developers' MIT licence named in its header.
- zebra_indexer.proto, Zebra's zebra-rpc/proto/indexer.proto through
  https://github.com/elicbarbieri/zebra at commit
  ebabeee504850bb9dc0b9a76de158dbc68503220: MIT OR Apache-2.0,
  zebra-LICENSE-MIT. The zebra-chain and zebra-network crates zingod links
  come from the same commit, under the same licences.

The C libraries linked in statically:
- AWS-LC 5.12.0 (aws-lc-sys 0.47.0): aws-lc-LICENSE.
- jemalloc 5.3.1 (tikv-jemalloc-sys 0.7.1): jemalloc-COPYING.
- zstd 1.5.7 (zstd-sys 2.1.0): zstd-LICENSE. zstd is
  BSD-3-Clause OR GPL-2.0, and this is its BSD licence.
- libsecp256k1 (secp256k1-sys 0.14.1): secp256k1-COPYING.
  The crate's own CC0-1.0 licence covers only its Rust wrapper.

aws-lc-LICENSE and jemalloc-COPYING are copied from the build's own sources
when a bundle is built; a development build doesn't have them.

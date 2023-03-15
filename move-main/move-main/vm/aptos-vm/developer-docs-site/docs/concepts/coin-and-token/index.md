---
title: "Follow the Aptos Standards"
slug: "index"
---

# Aptos Standards

Standards define a common interoperable interface for all developers to build upon. They consist of rules to ensure compatibility across applications and wallets on the Aptos blockchain.

## Digital Asset Standards
### [Aptos Coin](./aptos-coin.md)

The [Coin module](https://github.com/aptos-labs/aptos-core/blob/main/aptos-move/framework/aptos-framework/sources/coin.move) is a lightweight standard meant for simple, typesafe, and fungible assets. The coin standard is separated out into its own Move module to ensure that:
  - Applications and users can create and use simple tokens, with high performance and low gas overhead.
  - The Coin standard is part of the Aptos core framework so it can be used for currencies, including the gas currency.

See [Aptos Coin >](./aptos-coin.md)

### [Aptos Token](./aptos-token.md)

The [Token module](https://github.com/aptos-labs/aptos-core/blob/main/aptos-move/framework/aptos-token/sources/token.move), on the other hand:

- Encapsulates rich, flexible assets and collectibles. These assets are discrete (non-decimal) and can be fungible, semi-fungible, or nonfungible.
- The token standard is in its own `AptosToken` package at the Address `0x3` to allow for rapid iteration based on feedback from the community.

See [Aptos Token >](./aptos-token.md)

## Wallet standards
### [Aptos Wallet standards](../../guides/wallet-standard.md)

The wallet standards ensure that all wallets use the same functionality for key features.  This includes:
- The same mnemonic so that wallets can be moved between providers.
- [Wallet adapter](../../concepts/wallet-adapter-concept.md) so that all applications can interact seamlessly with a common interface.

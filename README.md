# sha-p2pool

This is a decentralized pool mining software for Tari network using SHA-3 algorithm.

How it works
---
This is an implementation of [Bitcoin's original p2pool](https://en.bitcoin.it/wiki/P2Pool) with some modifications.

Sha P2Pool uses an in-memory **side chain** (it is called **share chain**) to keep track of the miners
who contributed in mining
(only the last **80 blocks** are stored, so whoever get a share recently (mined a share chain block) will be included in
payouts!).

All the payouts are instant and decentralized without any authority to control them.

Prerequisites
---

- Running Tor proxy ([Instructions](https://github.com/tari-project/tari?tab=readme-ov-file#perform-sha3-mining))
- **Running** and **Synced** **Tari Base Node**
  ([Setup instructions (run minotari_node)](https://github.com/tari-project/tari))
- A Tari wallet address ([Setup instructions (run minotari_console_wallet)](https://github.com/tari-project/tari))

How to use
---

- First let's assume that we have all the [prerequisites](#Prerequisites) are set up and running
- Create a new configuration for the miner in `~/.tari/<NETWORK_HERE>/miner.toml`

  (where you should replace `<NETWORK_HERE>` with the current network the base node runs on, `esmeralda` is the
  default):
  ```toml
  [miner]
  base_node_grpc_address = "/ip4/127.0.0.1/tcp/18145"
  sha_p2pool_enabled = true
   ```

  Please note that `18145` is the port where `base node's gRPC` is running, so if it is different from the default
  one (`18145`)
  just use the right port.
- Start sha p2pool by running the binary simple `$ ./sha_p2pool start` or using `Cargo` (if installed and want to build
  from
  source):
  `$ cargo build --release && ./target/release/sha_p2pool start`

**Note:** For more information about usage of p2pool, just run `$ sha_p2pool --help`! 


# concierge.v1, vendored

`v1/auth.proto` and `v1/directory.proto` are copies, byte for byte, of
`contracts/proto/concierge/v1/` in [EV-invest/concierge](https://github.com/EV-invest/concierge)
at the commit in [`REV`](REV). The panel is a relying party of concierge: it calls
`AuthService.ExchangeCode` / `RefreshClientToken` and `UserDirectory.GetMe`, and
`crates/contracts` generates their tonic clients (and servers, for the tests' mock
concierge) from these files.

Copied rather than taken as the `evconcierge_contracts` git dependency banking uses, because that
crate's build runs `protoc`, which neither the CI runners nor the nix sandbox have; here protox
parses the protos in Rust, as it does for `sa/v1`.

Do not edit them here. To move to another concierge commit (after a concierge merge, pin to
its `main`):

```sh
contracts/proto/concierge/sync.sh <commit>
git diff contracts/proto/concierge   # what changed in the contract, then build and test
```

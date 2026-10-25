# scope.v1, vendored

`v1/invoices.proto`, `v1/events.proto`, `v1/service_auth.proto` and `v1/ledger.proto` are
copies, byte for byte, of `contracts/proto/scope/v1/` in
[EV-Main/banking](https://github.com/EV-Main/banking) at the commit in [`REV`](REV). The panel
is a scope client of banking's ScopeLedger listener in the scope `sa`: it calls
`ScopeAuthService.MintServiceToken`, `ScopeLedgerService.OpenAccount`,
`InvoiceService.CreateInvoice` / `GetInvoice` and `MoneyEvents.Pull`, and
`crates/panel_contracts` generates their tonic clients (and servers, for the tests' fake
ledger) from these files. `ledger.proto` comes along because `events.proto` imports it and
`OpenAccount` lives in it. `mandates.proto` is left out: nothing here imports or calls it.

Copied rather than taken as the `evbanking_contracts` git dependency, for the reason the
concierge protos are (`../concierge/README.md`): that crate's build runs `protoc`.

Do not edit them here. To move to another banking commit (after a banking merge, pin to its
`main`):

```sh
contracts/proto/scope/sync.sh <commit>
git diff contracts/proto/scope   # what changed in the contract, then build and test
```

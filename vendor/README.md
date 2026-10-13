# Vendored crates

Patched copies of dependencies, used through `[patch.crates-io]` in the workspace `Cargo.toml`.
Drop a copy once its fix is released upstream.

## outlook-pst 1.2.0

[microsoft/outlook-pst-rs](https://github.com/microsoft/outlook-pst-rs), MIT. The source is
unchanged (examples and dev-dependencies removed) except for these fixes, all marked
`FastFind patch`:

* `src/messaging/message.rs`: a message without a sub-node tree (no recipients, attachments
  or large properties, e.g. a note to self) was rejected with `MessageSubNodeTreeNotFound`; it
  is now read with no sub-nodes.
* `src/ltp/prop_type.rs`: property type `0x000D` (PtypObject) was not recognised. Reading a
  property list stops at the first entry that fails to parse, so an attachment holding an
  embedded message lost most of its properties and could not be opened
  (`AttachmentMethodNotFound`).
* `src/messaging/attachment.rs`: an embedded message was looked up among the parent message's
  sub-nodes; it is a node of the attachment's own sub-node tree ([MS-PST] 2.4.6.2). And it was
  read while the file lock was held, which deadlocked once the lookup succeeded: it is now read
  after the lock is released.

Together these made attached Outlook items unreadable; `tests/fixtures/outlook-archive.pst`
covers all of them.

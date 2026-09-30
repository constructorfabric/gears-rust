# CredStoreIntent: TLA+ model of the Vault/PG write-intent protocol

Model of one credential key held in Vault (system of record, monotonic store version)
and a Postgres index row (`ver`, `kind`, `val`, `intent`). Step ids in `CredStoreIntent.tla`
refer to `../cs-states.md` (R, W, C, D).

## Modelled
- Writers (create, update, delete): W.1 / W.1.3, W.3 (C.3.1/C.3.2), W.4 (incl. W.4.2.2), W.5;
  a crash is possible between any two steps.
- Read repair (R.3.1 / R.5.2).
- Fencing repair (W.7.3), started only when PG has an intent. Step 1 reads Vault and PG:
  - **W.7.3.0, Vault ahead of PG** (`PG.ver < Vault.ver`, NULL = 0 counts as below): no Vault write.
    One atomic PG update raises `ver/kind/val` to Vault's and clears the intent, guarded by
    `PG.ver < Vault.ver` on the current row. The repair is done. Every intent recorded at a
    version below Vault.ver belongs to a writer whose CAS can no longer land.
  - **W.7.3.1-W.7.3.3, otherwise** (`PG.ver = Vault.ver`, or Vault has no key): rewrite the same
    kind/val at `vk+1` conditioned on `Vault.ver = vk` (or a tombstone at ver 1 conditioned on
    absence) and remember the produced version. Then a separate step 2 does
    `UPDATE PG ... intent = FALSE WHERE PG.ver < produced`. The repairer may crash between the steps.
- Invariants: `TypeOK`, `I1_NoIntentMeansInSync` (strict: version, kind and value equal Vault's),
  `I2_IndexNotAhead`, `I3_NoHiddenRecord`, `I6_UnconfirmedCreateHasIntent`, `I7_ReadIsCorrect`;
  property `I5_VersionsNeverRepeat`.

## Not modelled
Offboarding / purge, expiry, multi-key hierarchy and inheritance, value timeouts
(W.4.3 unknown outcome), PG/Vault unavailability, client preconditions (`If-Match`).
W.1 is split into a PG read and a base step; each other step is atomic.

## Run
```
java -XX:+UseParallelGC -cp tla2tools.jar tlc2.TLC -workers auto -deadlock -metadir /some/tmp/dir CredStoreIntent
```
`-deadlock` because all writers may be idle or blocked at `MaxVer`; `-metadir` keeps TLC's
scratch files out of this directory. TLC 2.19. Configuration: `Writers = {w1, w2}`,
`Values = {a, b}`, `MaxVer = 4`.

## Result
"Model checking completed. No error has been found." 7,061,959 states generated,
1,241,286 distinct, depth 35, about 8 s. All invariants (with strict I1) and I5 hold.

Extra run (not in this directory): 3 writers, `MaxVer = 3`, stopped at the 10-minute limit
without finishing: 72,246,095 distinct states explored (509M generated, 4.4M still on the queue),
no violation found in the explored part. Not a complete proof for 3 writers.

## Finding from the first model run (fixed in the fencing repair)
The first version of the model had the fencing repair always rewrite Vault at `vk+1`. TLC found
`I1_NoIntentMeansInSync` violated: a create lands Vault v1, the repair rewrites v2 and stops before
its PG step, and then the create confirms v1 (`PG.ver < 1`), clearing the intent while PG is at v1
and Vault at v2. Rewriting Vault while it was already ahead of the index let a late confirm clear
the intent at a lagging version. The fix is the W.7.3.0 branch: when Vault is ahead of PG the
repair is PG-only and does not write Vault.

## Sanity mutation
On the current spec, Confirm was changed to clear the intent unconditionally (the `PG.ver < new`
guard dropped). TLC reports `I1_NoIntentMeansInSync` violated at depth 8 (3,370 states generated):
`w1` creates and lands Vault v1 without confirming, a read repair raises PG to v1 (intent kept), the
fencing repair rewrites Vault to v2 (PG still v1 with intent), and `w1`'s unguarded confirm clears
the intent with PG at v1 and Vault at v2. So the model catches the bug the `PG.ver < new` guard
avoids. The mutant is not kept in this directory.

------------------------- MODULE CredStoreIntent -------------------------
(***************************************************************************)
(* One credential key in two stores: Vault (system of record) and the      *)
(* Postgres (PG) index row with a write intent.  Step ids (R.x, W.x, C.x,  *)
(* D.x) refer to gears/credstore/docs/cs-states.md.                        *)
(* Not modelled: offboarding, expiry, multi-key hierarchy, value timeouts. *)
(***************************************************************************)
EXTENDS Naturals

CONSTANTS Writers, Values, MaxVer

NoVal == "none"                 \* value of a tombstone / absent record
Ops   == {"create", "update", "delete"}
Kinds == {"none", "live", "tomb"}

VARIABLES
    vault,   \* [ver, kind, val]; ver = 0 means "key absent"
    pg,      \* [ex, ver, kind, val, intent]; ver 0 = NULL (unconfirmed create)
    pc,      \* writer program counter
    op,      \* writer operation
    rv,      \* value carried by the request
    base,    \* writer base version (v1)
    snap,    \* writer's view of the PG row
    nv,      \* version produced by the writer's Vault write (v2)
    rpc,     \* fencing repairer pc: "idle" | "step2"
    rver     \* version produced by repairer step 1

vars == <<vault, pg, pc, op, rv, base, snap, nv, rpc, rver>>

VaultAbsent == [ver |-> 0, kind |-> "none", val |-> NoVal]
NoRow == [ex |-> FALSE, ver |-> 0, kind |-> "none", val |-> NoVal, intent |-> FALSE]
PCs == {"idle", "base", "set", "store", "confirm"}

Init ==
    /\ vault = VaultAbsent
    /\ pg = NoRow
    /\ pc = [w \in Writers |-> "idle"]
    /\ op = [w \in Writers |-> "none"]
    /\ rv = [w \in Writers |-> NoVal]
    /\ base = [w \in Writers |-> 0]
    /\ snap = [w \in Writers |-> NoRow]
    /\ nv = [w \in Writers |-> 0]
    /\ rpc = "idle"
    /\ rver = 0

\* Raise PG to the Vault state, intent untouched (R.3.1, W.1.3, W.4.2.2).
Raise(row) ==
    IF row.ex /\ row.ver < vault.ver
    THEN [row EXCEPT !.ver = vault.ver, !.kind = vault.kind, !.val = vault.val]
    ELSE row

\* Writer finishes (stop, reject, crash): all its state is reset.
Done(w) ==
    /\ pc'   = [pc   EXCEPT ![w] = "idle"]
    /\ op'   = [op   EXCEPT ![w] = "none"]
    /\ rv'   = [rv   EXCEPT ![w] = NoVal]
    /\ base' = [base EXCEPT ![w] = 0]
    /\ snap' = [snap EXCEPT ![w] = NoRow]
    /\ nv'   = [nv   EXCEPT ![w] = 0]

(* W.1 / C.1 / D.1: read PG *)
Start(w, o, v) ==
    /\ pc[w] = "idle"
    /\ o \in Ops
    /\ v \in (IF o = "delete" THEN {NoVal} ELSE Values)
    /\ pc'   = [pc   EXCEPT ![w] = "base"]
    /\ op'   = [op   EXCEPT ![w] = o]
    /\ rv'   = [rv   EXCEPT ![w] = v]
    /\ snap' = [snap EXCEPT ![w] = pg]
    /\ UNCHANGED <<vault, pg, base, nv, rpc, rver>>

Legal(o, r) ==
    IF o = "create"
    THEN (~r.ex) \/ (r.ver = 0 /\ r.intent) \/ r.kind = "tomb"   \* C.1.1/C.1.2/C.1.3
    ELSE r.ex /\ r.kind = "live"                                  \* W.1.1 / D.1

(* W.1.3: a row with an intent is first raised to the Vault state; then   *)
(* base := PG.ver.  Without an intent base is the version that was read.  *)
Base(w) ==
    /\ pc[w] = "base"
    /\ LET s == snap[w]
           useVault == s.ex /\ s.intent
           pg1 == IF useVault THEN Raise(pg) ELSE pg
           r   == IF useVault THEN pg1 ELSE s
       IN /\ pg' = pg1
          /\ IF Legal(op[w], r)
             THEN /\ pc'   = [pc   EXCEPT ![w] = "set"]
                  /\ base' = [base EXCEPT ![w] = r.ver]
                  /\ snap' = [snap EXCEPT ![w] = r]
                  /\ UNCHANGED <<op, rv, nv>>
             ELSE Done(w)            \* rejected, no state change (besides raise)
    /\ UNCHANGED <<vault, rpc, rver>>

(* W.3 / C.3 / D.3: set the intent *)
SetIntent(w) ==
    /\ pc[w] = "set"
    /\ UNCHANGED <<vault, rpc, rver>>
    /\ IF ~snap[w].ex
       THEN  \* C.3.1: INSERT; a unique violation (row appeared) stops
            IF pg.ex THEN /\ Done(w) /\ UNCHANGED pg
            ELSE /\ pg' = [ex |-> TRUE, ver |-> 0, kind |-> "live",
                           val |-> rv[w], intent |-> TRUE]
                 /\ pc' = [pc EXCEPT ![w] = "store"]
                 /\ UNCHANGED <<op, rv, base, snap, nv>>
       ELSE  \* UPDATE SET intent = TRUE WHERE ver = base
            IF pg.ex /\ pg.ver = base[w]
            THEN /\ pg' = IF op[w] = "create"     \* C.3.2 overwrite kind/val
                          THEN [pg EXCEPT !.intent = TRUE, !.kind = "live", !.val = rv[w]]
                          ELSE [pg EXCEPT !.intent = TRUE]
                 /\ pc' = [pc EXCEPT ![w] = "store"]
                 /\ UNCHANGED <<op, rv, base, snap, nv>>
            ELSE /\ Done(w) /\ UNCHANGED pg      \* 0 rows: conflict, stop

(* W.4 / C.4 / D.4: conditional Vault write *)
StoreWrite(w) ==
    /\ pc[w] = "store"
    /\ UNCHANGED <<rpc, rver>>
    /\ IF vault.ver = base[w]
       THEN \* W.4.1 success at v2 (disabled beyond MaxVer)
            /\ base[w] + 1 <= MaxVer
            /\ vault' = [ver |-> base[w] + 1,
                         kind |-> IF op[w] = "delete" THEN "tomb" ELSE "live",
                         val |-> IF op[w] = "delete" THEN NoVal ELSE rv[w]]
            /\ nv' = [nv EXCEPT ![w] = base[w] + 1]
            /\ pc' = [pc EXCEPT ![w] = "confirm"]
            /\ UNCHANGED <<pg, op, rv, base, snap>>
       ELSE \* W.4.2.2 conflict: raise PG, intent untouched, stop
            /\ pg' = Raise(pg)
            /\ UNCHANGED vault
            /\ Done(w)

\* what this writer wrote to Vault
vault_rec_kind(w) == IF op[w] = "delete" THEN "tomb" ELSE "live"
vault_rec_val(w)  == IF op[w] = "delete" THEN NoVal ELSE rv[w]

(* W.5 / C.5 / D.5: confirm, WHERE PG.ver < new (ver 0 is below all) *)
Confirm(w) ==
    /\ pc[w] = "confirm"
    /\ pg' = IF pg.ex /\ pg.ver < nv[w]
             THEN [pg EXCEPT !.ver = nv[w], !.intent = FALSE,
                   !.kind = vault_rec_kind(w), !.val = vault_rec_val(w)]
             ELSE pg
    /\ Done(w)
    /\ UNCHANGED <<vault, rpc, rver>>


(* Writer crash between any two steps: remaining steps never happen *)
Crash(w) ==
    /\ pc[w] # "idle"
    /\ Done(w)
    /\ UNCHANGED <<vault, pg, rpc, rver>>

(* R.3.1 / R.5.2: read repair; intent is left unchanged *)
ReadRepair ==
    /\ pg.ex /\ pg.ver < vault.ver
    /\ pg' = Raise(pg)
    /\ UNCHANGED <<vault, pc, op, rv, base, snap, nv, rpc, rver>>

(* W.7.3 fencing repair, step 1.  Reads both Vault and PG (intent TRUE).   *)
Fence1 ==
    /\ rpc = "idle"
    /\ pg.ex /\ pg.intent
    /\ IF vault.ver > 0 /\ pg.ver < vault.ver
       THEN \* W.7.3.0 Vault ahead of PG: PG-only repair, no Vault write.
            \* Every intent below Vault.ver belongs to a writer whose CAS can
            \* no longer land.  Single atomic step, guarded by PG.ver < Vault.ver.
            /\ pg' = [ex |-> TRUE, ver |-> vault.ver, kind |-> vault.kind,
                      val |-> vault.val, intent |-> FALSE]
            /\ UNCHANGED <<vault, rpc, rver>>
       ELSE \* W.7.3.1-2 rewrite (PG.ver = Vault.ver, or Vault has no key)
            /\ IF vault.ver > 0
               THEN /\ vault.ver + 1 <= MaxVer
                    /\ vault' = [vault EXCEPT !.ver = vault.ver + 1]  \* same kind/val
                    /\ rver' = vault.ver + 1
               ELSE /\ vault' = [ver |-> 1, kind |-> "tomb", val |-> NoVal] \* IfAbsent
                    /\ rver' = 1
            /\ rpc' = "step2"
            /\ UNCHANGED pg
    /\ UNCHANGED <<pc, op, rv, base, snap, nv>>

(* W.7.3.3: fencing repair, step 2 *)
Fence2 ==
    /\ rpc = "step2"
    /\ pg' = IF pg.ex /\ pg.ver < rver
             THEN [ex |-> TRUE, ver |-> vault.ver, kind |-> vault.kind,
                   val |-> vault.val, intent |-> FALSE]
             ELSE pg
    /\ rpc' = "idle" /\ rver' = 0
    /\ UNCHANGED <<vault, pc, op, rv, base, snap, nv>>

\* repairer crash between its two steps
FenceCrash ==
    /\ rpc = "step2"
    /\ rpc' = "idle" /\ rver' = 0
    /\ UNCHANGED <<vault, pg, pc, op, rv, base, snap, nv>>

Next ==
    \/ \E w \in Writers : \/ \E o \in Ops, v \in Values \cup {NoVal} : Start(w, o, v)
                          \/ Base(w) \/ SetIntent(w) \/ StoreWrite(w)
                          \/ Confirm(w) \/ Crash(w)
    \/ ReadRepair \/ Fence1 \/ Fence2 \/ FenceCrash

Spec == Init /\ [][Next]_vars

---------------------------------------------------------------------------
(* Invariants *)

TypeOK ==
    /\ vault \in [ver : 0..MaxVer, kind : Kinds, val : Values \cup {NoVal}]
    /\ pg \in [ex : BOOLEAN, ver : 0..MaxVer, kind : Kinds,
               val : Values \cup {NoVal}, intent : BOOLEAN]
    /\ pc \in [Writers -> PCs]
    /\ op \in [Writers -> Ops \cup {"none"}]
    /\ rv \in [Writers -> Values \cup {NoVal}]
    /\ base \in [Writers -> 0..MaxVer]
    /\ nv \in [Writers -> 0..MaxVer]
    /\ rpc \in {"idle", "step2"}
    /\ rver \in 0..MaxVer

I1_NoIntentMeansInSync ==
    /\ (pg.ex /\ ~pg.intent) =>
          (vault.ver > 0 /\ pg.ver = vault.ver /\ pg.kind = vault.kind
           /\ pg.val = vault.val)
    /\ (~pg.ex) => vault.ver = 0

I2_IndexNotAhead == pg.ver <= vault.ver

I3_NoHiddenRecord == (vault.ver > 0) => pg.ex

I6_UnconfirmedCreateHasIntent == (pg.ex /\ pg.ver = 0) => pg.intent

\* Reader rule (R.3 / R.5)
ReadResult ==
    IF pg.ex /\ pg.intent
    THEN IF vault.ver = 0 \/ vault.kind = "tomb" THEN "absent" ELSE vault.val
    ELSE IF ~pg.ex \/ pg.kind = "tomb" THEN "absent" ELSE pg.val

TruthFromVault ==
    IF vault.ver = 0 \/ vault.kind = "tomb" THEN "absent" ELSE vault.val

I7_ReadIsCorrect == ReadResult = TruthFromVault

I5_VersionsNeverRepeat == [][vault.ver' >= vault.ver]_vars
=============================================================================

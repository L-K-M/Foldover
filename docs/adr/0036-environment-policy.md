# ADR 0036: Environment policy

Status: Implemented

## Context

PLAN.md §3.5: automatic syncs should not quietly spend the Mac's battery
or a metered network. Two conditions matter: Low Power Mode, where the
person has already asked the Mac to save energy, and an expensive or
constrained network, where a sync could run up a cellular or otherwise
metered bill. Manual syncs are the person's explicit choice and never
wait on either.

The choice survives restarts, so it lives in the state database, not in
defaults the platform manages.

## Decision

- **Sources** (foldover-os): Low Power Mode is Foundation's
  `NSProcessInfo.isLowPowerModeEnabled`; the network's cost is a
  Network.framework path monitor's `isExpensive` and `isConstrained`
  (`constrained` is what the system reports while Low Data Mode is on).
  `Environment::now()` reads both once, and the scheduler takes one
  snapshot per tick so every due pair is judged against the same moment.
  A platform that cannot answer reports no network cost.
- **Holds:** `Environment::hold` answers whether an automatic sync waits.
  Low Power Mode holds every pair unless the person set the choice to
  run (`low_power: "run"`; the default and the no-row case are pause).
  An expensive or constrained network holds only pairs that use it: a
  pair uses the network when either root is `sftp://` or `peer://`, or
  a local root last synced from a share Foldover mounted (the recorded
  mount source). An unreadable mount record counts as network: on a
  costly network, automation fails closed.
- **Held pairs stay due:** a held candidate is skipped for that tick and
  nothing is recorded; it is not marked failed and does not wait for a
  person. It runs on the first tick after the hold lifts, which the next
  environment snapshot reports. Changes made during a hold are picked up
  then, like changes during a pause (ADR 0022).
- **Status:** `automation.status` answers `low_power` and `held_by`
  (`lowPowerMode`, `expensiveNetwork` or `constrainedNetwork`), the hold
  an automatic sync would get now: the network's cost when any pair uses
  it, else the low-power hold local pairs would get. `automation.settings`
  sets `low_power` and answers the status. Both ride local protocol 12.
- **Surface:** the menu bar shows the hold under the pause controls, and
  Settings carries "Sync automatically in Low Power Mode" (off is
  pause); `foldover automation` prints the status and
  `foldover automation low-power pause|run` sets the choice.

## Consequences

- Turning Low Power Mode off (or the choice on) starts pending
  automatic syncs on the next tick; nothing is lost or needs resuming.
- A pair whose local root was once a mounted share keeps counting as
  network until a sync observes it on plain disk again; a cautious
  reading errs toward waiting on metered links.
- The environment is sampled once per scheduler tick, so a hold that
  starts or ends between ticks is acted on within one tick of the OS
  reporting it.
- The status reports the hold an automatic sync would get, which during
  a manual pause may differ from what a person sees; the pause's own
  text takes the slot.

# Persisting Control

`persisting-control` owns the runtime control state machine and policy-driven
state transitions shared by pVisor execution points.

```text
Requested -> Allowed / Denied -> Applied / Failed
```

- `ControlRequest` is a typed resource request (currently network or model).
- `ControlController` evaluates policy and returns the authorization transition.
- `ControlMachine` validates transitions and retains the state/history.
- pVisor runtime drivers such as OverlayNet and Gateway apply the decision; this
  crate does not perform I/O.

An `Applied { effect: Deny }` state means the driver successfully blocked an
operation. It does not mean that a proxy-based driver is non-bypassable.

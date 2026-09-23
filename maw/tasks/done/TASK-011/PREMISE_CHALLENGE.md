## Counter-example tested

The registry treats two path spellings that identify the same folder on Windows (for example, case-only variants of `C:\\Work\\Project`) as different folders, so it can create multiple ordinal-1 slots and topics for one actual folder while every stated acceptance test still passes.

## Primary-source investigation

I inspected the ingress wire contract. `Register.cwd` is a `String` at `crates/cctg/src/wire.rs:112-118`. Hook paths are explicitly transported as strings because they may originate on another OS at `crates/cctg/src/wire.rs:322-333`, and `HookPost::new` stores the supplied `cwd` unchanged at `crates/cctg/src/wire.rs:336-353`.

I then ran this read-only identity probe against the real workspace:

```powershell
fsutil file queryfileid "C:\Users\user\dev\cctg"
fsutil file queryfileid "c:\USERS\USER\DEV\CCTG"
```

Its real output was:

```text
File ID is 0x000000000000000000c0000000025c9f
File ID is 0x000000000000000000c0000000025c9f
```

In the earlier path-string probe, `[string]::Equals($a,$b,[StringComparison]::Ordinal)` returned `False`, while both `Test-Path` calls returned `True`.

Finally, I checked what a child process reports after entering the case-variant path:

```powershell
$orig=(Get-Location).Path
Set-Location -LiteralPath 'c:\USERS\USER\DEV\CCTG'
python -c "import os; print(os.getcwd())"
Set-Location -LiteralPath $orig
```

The real output was `C:\USERS\USER\DEV\CCTG`, rather than the workspace spelling `C:\Users\user\dev\cctg`.

## Did it hold

Yes. The executable probes show that two ordinally different `cwd` strings can denote the same real Windows folder and that a child process can report the variant spelling. The current primary-source boundary transports and retains that spelling unchanged. Therefore a registry keyed by the received `(device, folder, ordinal)` strings can allocate two ordinal-1 slots for one actual folder, while the stated scenarios using a single spelling still pass. The premise does not define folder identity tightly enough for its success predicate to exclude this behavior.

## Verdict

PREMISE SUSPECT — `crates/cctg/src/wire.rs:112-118` and `crates/cctg/src/wire.rs:322-353` retain raw `cwd`, while the executed Windows probes above show distinct reported strings for the same existing folder ; smallest implied reframing: define and test stable, OS-aware folder identity before using `(device, folder, ordinal)` as the permanent slot key.

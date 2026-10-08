# How the NeedleVoice installer and uninstaller are built

Written after Windows Defender quarantined both `NeedleVoiceSetup.exe` and the
`Uninstall.exe` it had written into the install folder, reporting
`Trojan:Win32/Wacatac.B!ml` — a machine-learning heuristic, not a signature.

That report was a false positive, but it was not a random one. The build had a
shape that heuristics are trained to read as a dropper. This document records
what the shape was, what replaced it, and what must not be reintroduced.

## What was wrong

`do_install` did this:

```rust
let uninst = dir.join(nv_core::UNINSTALL_EXE);
if let Ok(me) = std::env::current_exe() {
    if me != uninst {
        std::fs::copy(&me, &uninst)?;   // the installer copies itself in
    }
}
```

and `do_uninstall`, running as that copy, finished with:

```rust
let script = format!("ping 127.0.0.1 -n 3 > nul & rmdir /s /q \"{}\"", dir.display());
Command::new("cmd").raw_arg(format!("/C {script}")).creation_flags(0x0800_0000).spawn();
```

Together that meant every installed copy of NeedleVoice contained a file with all
of the following properties:

1. **One executable, two identities.** `Uninstall.exe` was byte-for-byte the same
   file as `NeedleVoiceSetup.exe`. The same bytes appeared as an installer and as
   an uninstaller.
2. **A large unsigned executable carrying a compressed archive of other
   executables.** Setup was ~150 MB, nearly all of it a zstd-compressed tar of
   `NeedleVoice.exe`, `NeedleVoiceConfig.exe` and four DLLs, expanded to disk at
   run time. That is the textbook definition of a dropper: malware that installs
   other malware files from an embedded archive, without needing a network.
3. **A shell spawned to delete the folder it lives in.** A `cmd.exe` running
   `rmdir /s /q` on its own directory is a self-deletion pattern.
4. **Force-terminated other processes by name** (`taskkill /IM …`), which is what
   an installer does only when it is not being polite.
5. **No publisher identity.** `CompanyName`, `LegalCopyright` and
   `OriginalFilename` were blank in every binary.

The detection hit the *installer payload*, not the installed app: Defender
quarantined `…\Programs\Voxual\Uninstall.exe` (the pre-rename name of this
project), which is that self-copy. Deleting it broke the uninstall entry in Apps
& features, because the registry's `UninstallString` pointed at it.

Treat the list above as the design constraints. Any one of them on its own is
common and unremarkable; all five in one artifact is the false positive.

## What it is now

| | before | after |
|---|---|---|
| Uninstaller | a copy of the installer, as large as it | `crates/nv-uninstall`, its own program |
| Uninstaller size | ~150 MB (whole payload inside) | ~380 KB |
| Payload in the uninstaller | yes, the entire install | none |
| Self-deletion | `cmd.exe` + `rmdir /s /q` | rename aside, remove from a temporary copy |
| Stopping the agent | `taskkill /IM` | `WM_CLOSE`, then wait, terminate only if ignored |
| Publisher metadata | `ProductName` only | company, product, description, original name, copyright, versions |
| Application manifest | none | `asInvoker`, `PerMonitorV2`, declared supported OS |
| Uninstall entry | pointed at the file that got flagged | points at the uninstaller, which is not flagged |

### The uninstaller is a program of its own

`crates/nv-uninstall` builds `NeedleVoiceUninstall.exe`, which the installer
carries inside its payload and writes out as `Uninstall.exe`. It contains no
payload, creates no window, and writes no executable anywhere. `--uninstall`
handed to Setup is relayed to the installed uninstaller rather than implemented
there, which is why `nv-setup` has no uninstall code left to be copied.

### Nothing deletes itself

The uninstaller cannot delete the file it is running from, and the old workaround
— hand the job to `cmd.exe` — is the pattern that gets flagged. Instead:

1. `Uninstall.exe` copies itself to `%TEMP%\NeedleVoice-uninstall-<pid>\`;
2. launches that copy with `--parent <pid> --dir <install folder>` and exits;
3. the copy waits for the original process to end, then deletes the install
   folder — which is not the folder it is running from, so nothing is in use;
4. the copy renames its own image to `.old` and removes its scratch folder.

At no point does a program try to unlink its own running image, and no command
interpreter is involved.

If step 2 fails, the uninstaller renames itself to `Uninstall.exe.old` and
deletes everything else it can, which is the same trick used the same way, and
reports that the rest will go on a second run. It does not silently half-succeed.

### Stopping the running app

`nv_core::win::close_processes_named` enumerates the windows of processes with
the target image name, posts `WM_CLOSE`, and waits. The agent's tray window
handles `WM_CLOSE` by asking the listener to stop and posting a quit message, so
the microphone and speakers are released properly. Only a copy that ignores the
request is terminated. `crates/nv-agent/src/tray.rs` must keep those arms.

## Rules for changing this code

* **Never let Setup copy itself into the install folder.** Anything that needs
  to run from there belongs in the payload.
* **Never give the uninstaller an embedded payload or a compressed archive.**
  Its job is directory removal; it should stay small.
* **Never reach for `cmd.exe` to delete files.** Rename, or run from elsewhere.
* **Keep the version metadata complete.** `nv_version::apply` fills it in for
  every binary from one place; use it rather than setting fields by hand.
* **Keep `nv-uninstall` free of new dependencies.** Each one is more code in an
  artifact that is judged by its shape.

## Signing

The binaries are unsigned. The reference material on this class of false
positive is consistent that Authenticode signing is the single most effective
remedy, and that signing the outer executable while leaving the binaries inside
it unsigned can make matters worse — so if a certificate is added, every
executable and DLL in the payload is signed, not just Setup.

`scripts/build-installer.ps1 -Sign` will sign them when `NV_SIGN_CERT` names a
`.pfx` and `NV_SIGN_PASSWORD` its password. That is the point of the switch: it
makes "sign everything or sign nothing" the easy path.

## If Defender flags a build again

The heuristic is retrained on Microsoft's side and its verdicts change without
notice, so a clean build today is not a guarantee. When it happens:

1. Do not assume the build is fine. Check the artifact against the five
   properties at the top of this file, and check the source for a reintroduced
   self-copy or `cmd.exe` deletion.
2. Submit the file at
   [microsoft.com/wdsi/filesubmission](https://www.microsoft.com/en-us/wdsi/filesubmission)
   as a software developer and dispute the detection. Submissions are scanned
   immediately and can be tracked at
   [microsoft.com/wdsi/submissionhistory](https://www.microsoft.com/wdsi/submissionhistory).
3. Mention this document in the submission: the artifact is a per-user
   installer with a separate small uninstaller, signed or not, with the payload
   in an embedded archive.
4. Rebuild after signing if a certificate is available.

A build that changes a lot at once — a new toolchain, a big new dependency —
can move the artifact's shape enough to trip the classifier on its own. That is
another reason to keep the change in step 1 small: fewer new shapes at once.

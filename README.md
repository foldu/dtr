# dtr

A serial-first terminal: `libghostty-vt` for the VT core, egui/wgpu for the window, one
OS thread per pane. The chrome is the platform's own — Adwaita on Linux, the Windows 11
skin there — and the terminal is Kanagawa Wave.

<table>
  <tr>
    <td><img width="440" src="assets/readme/linux.png" alt="dtr on Linux: a bash session and a file pane, in the Adwaita look"></td>
    <td><img width="440" src="assets/readme/windows.png" alt="dtr on Windows 11: a PowerShell session and a file pane, in the Windows look"></td>
  </tr>
  <tr>
    <td align="center">Linux, Adwaita</td>
    <td align="center">Windows 11</td>
  </tr>
</table>

[AGENTS.md](AGENTS.md) is the rulebook, [PLAN.md](PLAN.md) the design and milestones, and
`docs/` the measured memos behind both. Regenerate the two frames with
`./scripts/screenshots.sh`.

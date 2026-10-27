# loom as the login screen (planned 2026-10-27, not built)

greetd was tried before (/etc/greetd: a Quickshell greeter on a throwaway Hyprland, launcher.sh):
two compositors one after the other, a hard cut (black, flicker, the GPU reopened) at every
handover, and the user found it bad. GDM is in use now.

## The idea

loom is the display manager. It starts at boot on the seat, shows the greeter, checks the password
through PAM, and starts the user's session in the same compositor: no second compositor, no cut.

- **The greeter in the shell's look**: the lock screen's clock and elements (ii's lock module, or a
  small QML greeter with the same pieces), user picker, password, session choice.
- **A seamless handover**: loom draws both, so the clock and the cards glide from the greeter into
  the desktop (the same animation curves as the lock screen), instead of a black frame.
- **Instant desktop**: while the password is typed, loom starts the user's shell (qs -c ii) and its
  services in a prepared session (after PAM succeeds only its windows are shown; on failure it is
  torn down). With the shell's QML cache (quickshell fork, qmlcache) the shell is up in well under
  a second anyway; started during the password it is there at once.

## Pieces

1. A loom mode `loom --greeter` run by a systemd unit on tty1 (as GDM is), as a `greeter` user,
   with logind seat access; PAM conversation through a small privileged helper (no PAM in the
   compositor process itself).
2. The session start: a logind session for the user (pam_systemd), the user's environment
   (systemd --user, dbus, XDG_RUNTIME_DIR), then the user's clients connect to the same loom.
   Hard part: loom runs as the greeter user before login; it must hand the seat and its Wayland
   socket over to the user's session (or run privileged and drop to the user per client).
3. The greeter UI and the handover animation.
4. Fallback: a key (or a failure) drops to GDM/agetty; never lock the user out.

## Risks

Login is the one place where a bug locks the user out: build and test it in a VM or the headless
runner first, keep GDM installed and switchable, and never enable it on the real seat without the
user doing it themselves.

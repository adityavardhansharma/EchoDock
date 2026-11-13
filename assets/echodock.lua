-- EchoDock: the dock, its shortcuts and its layer rule. Loaded from hyprland.lua.

-- Start the dock with the session.
o.launch_on_start("echodock")

-- The dock animates itself; Hyprland shouldn't fade or slide its surface too.
hl.layer_rule({ match = { namespace = "^echodock$" }, no_anim = true, animation = "none" })

-- Ctrl+Alt+1…9 opens or focuses the app in that dock slot; add Shift for a new window.
for i = 1, 9 do
  o.bind("CTRL + ALT + " .. i, "EchoDock: open app " .. i, "echodock ctl activate " .. i)
  o.bind("CTRL + ALT + SHIFT + " .. i, "EchoDock: new window of app " .. i, "echodock ctl new " .. i)
end

o.bind("SUPER + ALT + D", "EchoDock: toggle auto-hide", "echodock ctl autohide")
o.bind("SUPER + ALT + T", "EchoDock: show or hide the dock", "echodock ctl toggle")

-- Minimize to EchoDock on any workspace (Omarchy's only works on floating ones).
o.rebind("SUPER + M", "Minimize window to the dock", "echodock ctl minimize")

-- Omarchy's app grid lived in its dock; open the launcher EchoDock is set to use.
o.rebind("SUPER + ALT + SPACE", "Apps", "echodock ctl apps")

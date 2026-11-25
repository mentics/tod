ui.theme.strict("warn")
local ds = require("@ds")
ds.setup()
local store = require("@app/screens/chat/store")
local route = require("@app/screens/shell/route")
local Session = require("@app/screens/chat/session")
local Composer = require("@app/screens/chat/composer")
local sessions = require("@account/sessions")
local active = ui.shared("tod.chat.active", "")
local writable = ui.shared("tod.chat.writable", false)
local cwd = ui.shared("app.cwd", "")
local title = ui.shared("tod.chat.title", "")

ui.effect(function()
	local id = active:get()
	local path = cwd:get() or ""
	local name = title:get()
	if id ~= "" then
		sessions.now:set(({
			state = "ready",
			here = "mac",
			devices = { { id = "mac", name = "This computer", local_ = true, here = true, online = true } },
			projects = { { id = "p", name = name, device = "mac", path = path, git = true } },
			chats = { { id = id, title = name, project = "p", device = "mac", updated = 0, created = 0 } },
			places = { pinned = {}, sections = {} },
			places_ready = true,
			archived = {},
		}) :: any)
		route.open_session(id)
		store.open(id)
	end
end)

return ui.overlay {
	w_full = true,
	h_full = true,
	Session(active),
	ui.show(function() return writable:get() end, Composer()),
	require("@ds/menu_at").Host {},
	ds.toast.Host {},
}

-- Lionhold Export: records bags, bank and equipped gear for each character, and builds one export string
-- ("LHX1:" + base64) that Lionhold Armory reads, either pasted or from this addon's SavedVariables file.
-- Nothing leaves the game by itself; addons have no network access.

local FORMAT = "LHX1"
local NUM_BAGS = NUM_BAG_SLOTS or 4
local NUM_BANK_BAGS = NUM_BANKBAGSLOTS or 6
local BANK_MAIN = BANK_CONTAINER or -1

-- Forever's client may use the modern C_Container API or the old globals; support both.
local GetNumSlots = (C_Container and C_Container.GetContainerNumSlots) or GetContainerNumSlots

local function nameFromLink(link)
  return link and link:match("%[(.-)%]") or nil
end

local function slotItem(bag, slot)
  if C_Container and C_Container.GetContainerItemInfo then
    local info = C_Container.GetContainerItemInfo(bag, slot)
    if info and info.itemID then
      return info.itemID, info.stackCount or 1, info.quality or -1, info.itemName or nameFromLink(info.hyperlink) or ""
    end
  elseif GetContainerItemInfo then
    local _, count, _, quality, _, _, link, _, _, itemID = GetContainerItemInfo(bag, slot)
    if link then
      return itemID or tonumber(link:match("item:(%d+)")), count or 1, quality or -1, nameFromLink(link) or ""
    end
  end
end

local function clean(text)
  return (tostring(text or ""):gsub("[\t\n\r]", " "))
end

local function scanContainer(bag)
  local size = GetNumSlots(bag) or 0
  local items = {}
  for slot = 1, size do
    local id, count, quality, name = slotItem(bag, slot)
    if id then
      items[#items + 1] = table.concat({ slot, id, count, quality, clean(name) }, "\t")
    end
  end
  return { size = size, items = items }
end

local function character()
  LionholdExportDB = LionholdExportDB or {}
  local db = LionholdExportDB
  db.version = 1
  db.chars = db.chars or {}
  local guid = UnitGUID("player")
  if not guid then return nil end
  local c = db.chars[guid] or {}
  db.chars[guid] = c
  local _, classToken = UnitClass("player")
  local _, raceToken = UnitRace("player")
  c.name = UnitName("player")
  c.level = UnitLevel("player")
  c.class = classToken
  c.race = raceToken
  c.faction = UnitFactionGroup("player")
  c.realm = GetRealmName and GetRealmName() or ""
  -- The guild name isn't known for a few seconds after login; keep the last one we saw.
  local guild = GetGuildInfo and GetGuildInfo("player")
  if guild and guild ~= "" then c.guild = guild elseif c.guild == nil then c.guild = "" end
  return c
end

-- Right after login and during logout the game reports empty bags (size 0). Such a read is never saved, so it can't
-- replace real data. The backpack always has 16 slots once inventory has loaded.
local function scanBags()
  if (GetNumSlots(0) or 0) == 0 then return false end
  local c = character()
  if not c then return false end
  local bags = {}
  for bag = 0, NUM_BAGS do
    bags[bag + 1] = scanContainer(bag)
    bags[bag + 1].bag = bag
  end
  c.bags = bags
  c.bagsAt = time()
  return true
end

-- The bank can only be read while the bank window is open, so it is saved on each visit.
local function scanBank()
  local main = scanContainer(BANK_MAIN)
  if main.size == 0 then return false end
  local c = character()
  if not c then return false end
  main.bag = BANK_MAIN
  local bank = { main }
  for bag = NUM_BAGS + 1, NUM_BAGS + NUM_BANK_BAGS do
    local scanned = scanContainer(bag)
    scanned.bag = bag
    bank[#bank + 1] = scanned
  end
  c.bank = bank
  c.bankAt = time()
  return true
end

local function scanEquipment()
  local equipped = {}
  for slot = 1, 19 do
    local link = GetInventoryItemLink("player", slot)
    local id = GetInventoryItemID and GetInventoryItemID("player", slot) or (link and tonumber(link:match("item:(%d+)")))
    if id then
      local quality = GetInventoryItemQuality and GetInventoryItemQuality("player", slot) or -1
      equipped[#equipped + 1] = table.concat({ slot, id, quality or -1, clean(nameFromLink(link)) }, "\t")
    end
  end
  -- Nothing equipped at all means the game hasn't loaded gear yet (or is logging out).
  if #equipped == 0 then return false end
  local c = character()
  if not c then return false end
  c.equipped = equipped
  c.equipAt = time()
  return true
end

-- Gold, in copper. Read during play only (like bags, it isn't trustworthy while logging out).
local function scanMoney()
  local money = GetMoney and GetMoney()
  if money == nil then return false end
  local c = character()
  if not c then return false end
  c.money = money
  c.moneyAt = time()
  return true
end

local B64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
local function base64(data)
  local out = {}
  for i = 1, #data, 3 do
    local a, b, c = data:byte(i, i + 2)
    local n = a * 65536 + (b or 0) * 256 + (c or 0)
    local c1 = math.floor(n / 262144) % 64
    local c2 = math.floor(n / 4096) % 64
    local c3 = math.floor(n / 64) % 64
    local c4 = n % 64
    out[#out + 1] = B64:sub(c1 + 1, c1 + 1)
      .. B64:sub(c2 + 1, c2 + 1)
      .. (b and B64:sub(c3 + 1, c3 + 1) or "=")
      .. (c and B64:sub(c4 + 1, c4 + 1) or "=")
  end
  return table.concat(out)
end

-- One line per record, tab separated:
--   C guid name level class race faction realm guild bagsAt bankAt equipAt money moneyAt
--   K where(bags|bank) bagId size        (a container)
--   I where bagId slot itemId count quality name
--   E slot itemId quality name
local function buildExport()
  local db = LionholdExportDB
  if not db or not db.chars then return nil end
  local lines = { FORMAT }
  for guid, c in pairs(db.chars) do
    lines[#lines + 1] = table.concat({ "C", guid, clean(c.name), c.level or 0, c.class or "", c.race or "", c.faction or "",
      clean(c.realm), clean(c.guild), c.bagsAt or 0, c.bankAt or 0, c.equipAt or 0, c.money or -1, c.moneyAt or 0 }, "\t")
    for _, where in ipairs({ "bags", "bank" }) do
      for _, container in ipairs(c[where] or {}) do
        lines[#lines + 1] = table.concat({ "K", where, container.bag, container.size }, "\t")
        for _, item in ipairs(container.items or {}) do
          lines[#lines + 1] = "I\t" .. where .. "\t" .. container.bag .. "\t" .. item
        end
      end
    end
    for _, item in ipairs(c.equipped or {}) do
      lines[#lines + 1] = "E\t" .. item
    end
  end
  db.export = FORMAT .. ":" .. base64(table.concat(lines, "\n"))
  db.exportAt = time()
  return db.export
end

local function scanAll()
  scanBags()
  scanEquipment()
  scanMoney()
end

-- A small window with the export string selected, ready to copy (Ctrl/Cmd+C).
local exportFrame
local function showExport()
  scanAll()
  local text = buildExport() or ""
  if not exportFrame then
    exportFrame = CreateFrame("Frame", "LionholdExportFrame", UIParent, BackdropTemplateMixin and "BackdropTemplate" or nil)
    exportFrame:SetSize(520, 220)
    exportFrame:SetPoint("CENTER")
    exportFrame:SetFrameStrata("DIALOG")
    exportFrame:EnableMouse(true)
    exportFrame:SetMovable(true)
    exportFrame:RegisterForDrag("LeftButton")
    exportFrame:SetScript("OnDragStart", exportFrame.StartMoving)
    exportFrame:SetScript("OnDragStop", exportFrame.StopMovingOrSizing)
    if exportFrame.SetBackdrop then
      exportFrame:SetBackdrop({
        bgFile = "Interface\\DialogFrame\\UI-DialogBox-Background",
        edgeFile = "Interface\\DialogFrame\\UI-DialogBox-Border",
        tile = true, tileSize = 32, edgeSize = 32,
        insets = { left = 11, right = 12, top = 12, bottom = 11 },
      })
    end
    local title = exportFrame:CreateFontString(nil, "OVERLAY", "GameFontNormal")
    title:SetPoint("TOP", 0, -18)
    title:SetText("Lionhold Export: press Ctrl+C (Cmd+C on Mac) to copy")
    local scroll = CreateFrame("ScrollFrame", "LionholdExportScroll", exportFrame, "UIPanelScrollFrameTemplate")
    scroll:SetPoint("TOPLEFT", 20, -42)
    scroll:SetPoint("BOTTOMRIGHT", -40, 46)
    local box = CreateFrame("EditBox", nil, scroll)
    box:SetMultiLine(true)
    box:SetFontObject(ChatFontNormal)
    box:SetWidth(440)
    box:SetAutoFocus(true)
    box:SetScript("OnEscapePressed", function() exportFrame:Hide() end)
    scroll:SetScrollChild(box)
    exportFrame.box = box
    local close = CreateFrame("Button", nil, exportFrame, "UIPanelButtonTemplate")
    close:SetSize(100, 24)
    close:SetPoint("BOTTOM", 0, 16)
    close:SetText(CLOSE or "Close")
    close:SetScript("OnClick", function() exportFrame:Hide() end)
  end
  exportFrame.box:SetText(text)
  exportFrame:Show()
  exportFrame.box:SetFocus()
  exportFrame.box:HighlightText()
end

local function after(seconds, fn)
  if C_Timer and C_Timer.After then C_Timer.After(seconds, fn) else fn() end
end

local frame = CreateFrame("Frame")
frame:RegisterEvent("PLAYER_ENTERING_WORLD")
frame:RegisterEvent("BAG_UPDATE_DELAYED")
frame:RegisterEvent("PLAYER_EQUIPMENT_CHANGED")
frame:RegisterEvent("BANKFRAME_OPENED")
frame:RegisterEvent("PLAYERBANKSLOTS_CHANGED")
frame:RegisterEvent("PLAYER_GUILD_UPDATE")
frame:RegisterEvent("PLAYER_MONEY")
frame:RegisterEvent("PLAYER_LOGOUT")
local bankOpen = false
frame:SetScript("OnEvent", function(_, event)
  if event == "PLAYER_ENTERING_WORLD" then
    -- Inventory and guild info arrive a moment after entering the world.
    after(3, scanAll)
    after(15, scanAll)
  elseif event == "BAG_UPDATE_DELAYED" then
    scanBags()
    if bankOpen then scanBank() end
  elseif event == "PLAYER_EQUIPMENT_CHANGED" then
    scanEquipment()
  elseif event == "PLAYER_GUILD_UPDATE" then
    character()
  elseif event == "PLAYER_MONEY" then
    scanMoney()
  elseif event == "BANKFRAME_OPENED" then
    bankOpen = true
    after(0.5, scanBank)
  elseif event == "PLAYERBANKSLOTS_CHANGED" then
    if bankOpen then scanBank() end
  elseif event == "PLAYER_LOGOUT" then
    -- Don't scan here: the game has already emptied the inventory. Just package what was read during play;
    -- SavedVariables are written right after this event.
    buildExport()
  end
end)
-- BANKFRAME_CLOSED is registered separately so a missing event on some clients can't break the rest.
pcall(frame.RegisterEvent, frame, "BANKFRAME_CLOSED")
frame:HookScript("OnEvent", function(_, event)
  if event == "BANKFRAME_CLOSED" then bankOpen = false end
end)

SLASH_LIONHOLD1 = "/lionhold"
SlashCmdList.LIONHOLD = function(msg)
  msg = (msg or ""):lower():match("^%s*(.-)%s*$")
  if msg == "export" then
    showExport()
  elseif msg == "debug" then
    local api = (C_Container and C_Container.GetContainerItemInfo) and "C_Container" or (GetContainerItemInfo and "global" or "none")
    local sizes = {}
    for bag = 0, NUM_BAGS do sizes[#sizes + 1] = tostring(GetNumSlots(bag) or "nil") end
    local c = character()
    print("|cffd4a84aLionhold Export debug|r: API " .. api .. ", bag sizes " .. table.concat(sizes, "/") ..
      ", saved bags " .. ((c and c.bagsAt) and date("%H:%M:%S", c.bagsAt) or "never") ..
      ", bank " .. ((c and c.bankAt) and date("%H:%M:%S", c.bankAt) or "never") ..
      ", gear " .. ((c and c.equipAt) and date("%H:%M:%S", c.equipAt) or "never") ..
      ", guild " .. ((c and c.guild ~= "" and c.guild) or "none"))
  else
    local c = character()
    local db = LionholdExportDB
    local count = 0
    for _ in pairs(db and db.chars or {}) do count = count + 1 end
    print("|cffd4a84aLionhold Export|r: " .. count .. " character(s) saved.")
    if c and not c.bankAt then
      print("Visit a banker once on this character so its bank is included.")
    end
    print("Type |cffffffff/lionhold export|r to copy your data, or upload the SavedVariables file at lionhold-armory.drodenberg92.workers.dev/me/bags after logging out.")
  end
end

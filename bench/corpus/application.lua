-- Entity updates combine tables, methods, captured state, and formatted events.
local function make_update()
  local hits = 0
  return function(self)
    self.x = self.x + self.vx
    self.y = self.y + self.vy
    if self.x > 500 or self.x < -500 then self.vx = -self.vx; hits = hits + 1 end
    if self.y > 500 or self.y < -500 then self.vy = -self.vy; hits = hits + 1 end
    return hits
  end
end
local entities = {}
for i = 1, 256 do
  entities[i] = {
    x = i, y = -i, vx = i % 7 + 1, vy = i % 5 + 1,
    name = "entity-" .. i, update = make_update(),
  }
end
local events = {}
local sum, bytes = 0, 0
for frame = 1, 12000 do
  for i, entity in ipairs(entities) do
    sum = sum + entity:update()
    if frame % 100 == 0 then
      local event = string.format("%s:%d,%d", entity.name, entity.x, entity.y)
      events[i] = event
      bytes = bytes + #event
    end
  end
end
local positions = 0
for _, entity in ipairs(entities) do positions = positions + entity.x + entity.y end
print("application", sum, bytes, positions, #table.concat(events, ";"))

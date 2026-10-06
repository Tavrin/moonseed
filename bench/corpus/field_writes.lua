local t = {x=0, y=0, z=0}
for i = 1, 18000000 do t.x = i; t.y = i + 1; t.z = i + 2 end
print("field_writes", t.x + t.y + t.z)

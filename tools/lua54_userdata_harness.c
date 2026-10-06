/* Lua 5.4.9 with the userdata natives Moonseed's proof host registers:
   newud(size [, nuv]), light(n), udpeek(u, i), udpoke(u, i, b), so the
   userdata and GC corpora (crates/moonseed/fixtures/lua/corpus_userdata.lua,
   corpus_gc.lua) run on both. Warnings print to stdout as
   "[warn] text", one line per warning, as Moonseed's tests print them.
   Lua source cannot make userdata; this harness makes them through Lua's
   C API. Build against a Lua 5.4.9 source tree:

       cc -O2 -I<lua>/src tools/lua54_userdata_harness.c <lua>/src/liblua.a -lm -ldl -o luaud

   then MOONSEED_LUA54_UD=./luaud cargo test -p moonseed --lib
   lua54_oracle -- --ignored. Written for Moonseed; no Lua test code. */
#include <stdio.h>
#include <stdint.h>
#include <string.h>
#include "lua.h"
#include "lauxlib.h"
#include "lualib.h"

static int newud(lua_State *L) {
  lua_Integer size = luaL_checkinteger(L, 1);
  lua_Integer nuv = luaL_optinteger(L, 2, 0);
  void *p = lua_newuserdatauv(L, (size_t)size, (int)nuv);
  memset(p, 0, (size_t)size);
  return 1;
}
static int light(lua_State *L) {
  lua_pushlightuserdata(L, (void *)(uintptr_t)luaL_checkinteger(L, 1));
  return 1;
}
static int udpeek(lua_State *L) {
  unsigned char *p = lua_touserdata(L, 1);
  lua_pushinteger(L, p[luaL_checkinteger(L, 2)]);
  return 1;
}
static int udpoke(lua_State *L) {
  unsigned char *p = lua_touserdata(L, 1);
  p[luaL_checkinteger(L, 2)] = (unsigned char)luaL_checkinteger(L, 3);
  return 0;
}
/* Warnings to stdout, one line each, as Moonseed's runner prints them. */
static char warnbuf[1 << 16];
static size_t warnlen = 0;
static void warnf(void *ud, const char *msg, int tocont) {
  size_t n = strlen(msg);
  (void)ud;
  if (warnlen + n < sizeof(warnbuf)) { memcpy(warnbuf + warnlen, msg, n); warnlen += n; }
  if (!tocont) { printf("[warn] %.*s\n", (int)warnlen, warnbuf); warnlen = 0; }
}
int main(int argc, char **argv) {
  lua_State *L = luaL_newstate();
  luaL_openlibs(L);
  lua_setwarnf(L, warnf, NULL);
  lua_register(L, "newud", newud);
  lua_register(L, "light", light);
  lua_register(L, "udpeek", udpeek);
  lua_register(L, "udpoke", udpoke);
  if (luaL_dofile(L, argv[1]) != LUA_OK) {
    fprintf(stderr, "lua: %s\n", lua_tostring(L, -1));
    return 1;
  }
  lua_close(L);
  return 0;
}

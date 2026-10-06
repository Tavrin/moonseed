/* Frozen hook oracle, Lua 5.4.9 without LUA_COMPAT_5_3.
 * Build (output belongs in the lane's results directory):
 * cc -O2 -Wall -Wextra -Werror -I<lua>/src tools/lua54_hook_harness.c \
 *    <lua>/src/liblua.a -lm -ldl -o <results>/lua54-hooks
 * Run a relative driver filename under the corpus runner's process caps.
 * Native contract and record schema: tests/hooks/README.md.
 */
#include <stdio.h>
#include <string.h>
#include "lua.h"
#include "lauxlib.h"
#include "lualib.h"

static const char statekey;
static const char *const events[] = {"call", "return", "line", "count", "tail call"};

static lua_State *threadarg(lua_State *L, int *arg) {
  if (lua_isthread(L, 1)) { *arg = 2; return lua_tothread(L, 1); }
  *arg = 1;
  return L;
}

/* Registry ephemeron: a thread is not kept alive merely by installation. */
static void states(lua_State *L) {
  lua_rawgetp(L, LUA_REGISTRYINDEX, &statekey);
  if (!lua_isnil(L, -1)) return;
  lua_pop(L, 1);
  lua_newtable(L);
  lua_newtable(L);
  lua_pushliteral(L, "k"); lua_setfield(L, -2, "__mode");
  lua_setmetatable(L, -2);
  lua_pushvalue(L, -1); lua_rawsetp(L, LUA_REGISTRYINDEX, &statekey);
}

static void integer(lua_State *L, const char *key, lua_Integer v) {
  lua_pushinteger(L, v); lua_setfield(L, -2, key);
}
static void string(lua_State *L, const char *key, const char *v) {
  if (v) lua_pushstring(L, v); else lua_pushnil(L);
  lua_setfield(L, -2, key);
}
static void boolean(lua_State *L, const char *key, int v) {
  lua_pushboolean(L, v); lua_setfield(L, -2, key);
}

static void hook(lua_State *L, lua_Debug *ar) {
  int top = lua_gettop(L), depth = 0, mode, records, row;
  lua_Debug frame;
  states(L);
  lua_pushthread(L); lua_rawget(L, -2);
  if (!lua_istable(L, -1)) { lua_settop(L, top); return; }
  lua_getfield(L, -1, "yieldmode"); mode = (int)lua_tointeger(L, -1); lua_pop(L, 1);
  lua_getfield(L, -1, "records"); records = lua_gettop(L);
  if (lua_rawlen(L, records) >= 20000) luaL_error(L, "C hook record limit");
  lua_getinfo(L, "nSltur", ar);
  lua_newtable(L); row = lua_gettop(L);
  string(L, "event", events[ar->event]);
  if (ar->event == LUA_HOOKLINE && ar->currentline >= 0) integer(L, "line", ar->currentline);
  string(L, "name", ar->name); string(L, "namewhat", ar->namewhat);
  string(L, "what", ar->what); string(L, "short_src", ar->short_src);
  integer(L, "currentline", ar->currentline); integer(L, "linedefined", ar->linedefined);
  boolean(L, "istailcall", ar->istailcall);
  integer(L, "ftransfer", ar->ftransfer); integer(L, "ntransfer", ar->ntransfer);
  integer(L, "nparams", ar->nparams); boolean(L, "isvararg", ar->isvararg);
  while (lua_getstack(L, depth, &frame)) depth++;
  integer(L, "depth", depth);
  lua_newtable(L);
  if (ar->event == LUA_HOOKCALL || ar->event == LUA_HOOKRET) {
    unsigned int i;
    for (i = 0; i < ar->ntransfer; i++) {
      const char *name = lua_getlocal(L, ar, ar->ftransfer + (int)i);
      lua_newtable(L);
      string(L, "name", name);
      if (name) { lua_pushvalue(L, -2); lua_setfield(L, -2, "value"); }
      lua_rawseti(L, -2 - (name != NULL), i + 1);
      if (name) lua_pop(L, 1);
    }
  }
  lua_setfield(L, row, "transfers");
  lua_rawseti(L, records, (lua_Integer)lua_rawlen(L, records) + 1);
  lua_settop(L, top);
  if ((mode & (1 << ar->event)) != 0) lua_yield(L, 0);
}

static int chook(lua_State *L) {
  int arg, mask = 0, mode = 0;
  lua_State *target = threadarg(L, &arg);
  const char *s = luaL_checkstring(L, arg);
  int count = (int)luaL_checkinteger(L, arg + 1);
  const char *yieldmode = luaL_checkstring(L, arg + 2);
  if (strchr(s, 'c')) mask |= LUA_MASKCALL;
  if (strchr(s, 'r')) mask |= LUA_MASKRET;
  if (strchr(s, 'l')) mask |= LUA_MASKLINE;
  if (count > 0) mask |= LUA_MASKCOUNT;
  if (!strcmp(yieldmode, "line")) mode = 1 << LUA_HOOKLINE;
  else if (!strcmp(yieldmode, "count")) mode = 1 << LUA_HOOKCOUNT;
  else if (!strcmp(yieldmode, "both")) mode = (1 << LUA_HOOKLINE) | (1 << LUA_HOOKCOUNT);
  else if (!strcmp(yieldmode, "call")) mode = 1 << LUA_HOOKCALL;
  else if (!strcmp(yieldmode, "return")) mode = 1 << LUA_HOOKRET;
  else if (strcmp(yieldmode, "none")) return luaL_argerror(L, arg + 2, "invalid yieldmode");
  states(L);
  lua_pushthread(target);
  if (target != L) lua_xmove(target, L, 1);
  lua_newtable(L);
  integer(L, "yieldmode", mode);
  lua_newtable(L); lua_setfield(L, -2, "records");
  lua_pushvalue(L, -1); lua_insert(L, -3); /* state, key, state */
  lua_rawset(L, -4); /* states, state */
  lua_getfield(L, -1, "records");
  lua_sethook(target, hook, mask, count);
  return 1;
}

static int chookget(lua_State *L) {
  int arg;
  lua_State *target = threadarg(L, &arg);
  lua_settop(L, arg == 2 ? 1 : 0);
  lua_getglobal(L, "debug"); lua_getfield(L, -1, "gethook"); lua_remove(L, -2);
  lua_pushthread(target);
  if (target != L) lua_xmove(target, L, 1);
  lua_call(L, 1, LUA_MULTRET);
  return lua_gettop(L) - (arg == 2 ? 1 : 0);
}

static int chookoff(lua_State *L) {
  int arg;
  lua_State *target = threadarg(L, &arg);
  lua_sethook(target, NULL, 0, 0);
  return 0;
}

int main(int argc, char **argv) {
  lua_State *L;
  int status;
  if (argc != 2) { fprintf(stderr, "usage: lua54-hooks FILE\n"); return 2; }
  L = luaL_newstate();
  if (!L) return 2;
  luaL_openlibs(L);
  lua_register(L, "chook", chook); lua_register(L, "chookget", chookget);
  lua_register(L, "chookoff", chookoff);
  status = luaL_dofile(L, argv[1]);
  if (status != LUA_OK) fprintf(stderr, "lua: %s\n", lua_tostring(L, -1));
  lua_close(L);
  return status == LUA_OK ? 0 : 1;
}

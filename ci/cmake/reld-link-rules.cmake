# Loaded through CMAKE_USER_MAKE_RULES_OVERRIDE after CMake's platform files.
#
# Some CMake platform modules add their own `-fuse-ld=` option after ordinary
# linker flags. In particular, Windows-Clang appends `-fuse-ld=lld`, which can
# silently override a caller-provided `-fuse-ld=reld-link`. Put the reld
# selector at the very end of each compiler-driver link command so the tested
# linker is the one Clang actually invokes.

set(RELD_LINKER_SELECTOR "$ENV{RELD_LINKER_SELECTOR}")
if(RELD_LINKER_SELECTOR STREQUAL "")
  message(FATAL_ERROR "RELD_LINKER_SELECTOR must select the reld linker under test")
endif()

set(
  CMAKE_C_LINK_EXECUTABLE
  "<CMAKE_C_COMPILER> <FLAGS> <LINK_FLAGS> <OBJECTS> -o <TARGET> <LINK_LIBRARIES> -fuse-ld=${RELD_LINKER_SELECTOR}"
)
set(
  CMAKE_CXX_LINK_EXECUTABLE
  "<CMAKE_CXX_COMPILER> <FLAGS> <LINK_FLAGS> <OBJECTS> -o <TARGET> <LINK_LIBRARIES> -fuse-ld=${RELD_LINKER_SELECTOR}"
)

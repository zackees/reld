// Companion object for the deplibs-missing config: names a library that cannot exist, so the link
// must fail with lld's wording.
#pragma comment(lib, "reld_deplibs_does_not_exist")
int reld_deplibs_missing_marker(void) { return 0; }

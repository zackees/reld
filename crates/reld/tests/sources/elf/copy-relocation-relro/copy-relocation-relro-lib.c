// Exports one read-only and one writable object. A non-PIE executable that references both takes
// a copy relocation for each, and only the read-only one may keep its protection.
const int ro_value = 7;
int rw_value = 8;

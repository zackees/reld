//#CompArgs:-fPIC
//#RunEnabled:false
// GNU ld discards foo. Reld, like lld doesn't.
//#ReferenceLinkers:lld
//#LinkArgs:--shared -znow ./versioned-script-symbol.map
//#ExpectSym:mysql_affected_rows@libmysqlclient_18
//#DiffIgnore:section.gnu.version_d.alignment #13
//#DiffIgnore:version_d.verdef_1 #13
// TODO: Look into this. Neither GNU ld nor lld emit this dynsym.
//#DiffIgnore:dynsym.mysql_affected_rows.section #13

int foo(void) { return 42; }

int custom_alpha __attribute__((section(".reld.custom.alpha"))) = 7;
int custom_beta __attribute__((section(".reld.custom.beta"))) = 35;

int custom_section_sum(void) { return custom_alpha + custom_beta; }

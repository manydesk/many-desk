// ManyDesk for iOS: everything is the Rust library (libmanydesk.a); winit runs UIKit's
// application loop from manydesk_main, which never returns.
extern void manydesk_main(void);

int main(int argc, char *argv[]) {
    (void)argc;
    (void)argv;
    manydesk_main();
    return 0;
}

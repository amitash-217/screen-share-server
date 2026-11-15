//! The real window, headless: switching screens must not panic.

slint::include_modules!();

use slint::{ComponentHandle, ModelRc, VecModel};

#[test]
fn every_screen_and_the_room_view_render() {
    i_slint_backend_testing::init_no_event_loop();
    let ui = MainWindow::new().unwrap();
    ui.show().unwrap();

    ui.set_rooms(ModelRc::new(VecModel::from(vec![RoomItem {
        code: "abcde".into(),
        host: "me".into(),
        permanent: false,
        mine: true,
    }])));
    for screen in 0..=2 {
        ui.set_screen(screen);
    }

    // As `render` fills it when a room is entered.
    ui.set_input_devices(ModelRc::new(VecModel::from(vec!["Mic".into()])));
    ui.set_output_devices(ModelRc::new(VecModel::from(vec!["Speakers".into()])));
    ui.set_peers(ModelRc::new(VecModel::from(vec![PeerItem {
        username: "bob".into(),
        speaking: true,
        state: "connected".into(),
        sharing: true,
    }])));
    ui.set_shares(ModelRc::new(VecModel::from(vec![ShareItem { username: "bob".into(), video: Default::default() }])));
    ui.set_room_code("abcde".into());
    ui.set_screen(3);
    ui.set_sharing(true);
    ui.set_muted(true);
    ui.set_screen(2);
}

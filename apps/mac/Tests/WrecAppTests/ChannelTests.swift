import Testing
@testable import wrec_app

struct ChannelTests {
    @Test
    func channelIdentitiesDoNotOverlap() {
        #expect(WrecChannel.allCases.map(\.appName) == ["Wrec Dev", "Wrec Nightly", "Wrec"])
        #expect(WrecChannel.allCases.map(\.homeName) == [".wrec-dev", ".wrec-nightly", ".wrec"])
        #expect(WrecChannel.allCases.map(\.cliName) == ["wrec-dev", "wrec-nightly", "wrec"])
        #expect(
            WrecChannel.allCases.map(\.jobChangedNotification)
                == [
                    "app.wrec.dev.job-changed",
                    "app.wrec.nightly.job-changed",
                    "app.wrec.release.job-changed",
                ])
    }
}

import Testing

@testable import wrec_app

struct UpdaterVersionTests {
    @Test
    func plainSemverOrdering() {
        #expect(Updater.isNewer("0.3.0", than: "0.2.0"))
        #expect(Updater.isNewer("1.0.0", than: "0.9.9"))
        #expect(!Updater.isNewer("0.2.0", than: "0.2.0"))
        #expect(!Updater.isNewer("0.1.9", than: "0.2.0"))
    }

    @Test
    func suffixedTagsCompareByTheirNumericPrefix() {
        // A suffixed tag equal to or older than the installed version must
        // not be offered as an update on every launch.
        #expect(!Updater.isNewer("0.2.0-rc1", than: "0.2.0"))
        #expect(!Updater.isNewer("0.1.0-hotfix", than: "0.2.0"))
        #expect(Updater.isNewer("0.3.0-rc1", than: "0.2.0"))
    }

    @Test
    func unparseableVersionsFallBackToInequality() {
        #expect(Updater.isNewer("nightly", than: "0.2.0"))
        #expect(!Updater.isNewer("0.2.0", than: "0.2.0"))
    }
}

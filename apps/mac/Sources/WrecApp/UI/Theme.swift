// wrec's visual identity in the native shell: system chrome everywhere the
// OS is better, Departure Mono pixel type everywhere wrec is speaking, and
// the wrec red reserved — as always — for record/live.

import CoreText
import SwiftUI

enum Theme {
    static let red = Color(red: 0xC6 / 255.0, green: 0x28 / 255.0, blue: 0x28 / 255.0)

    /// Registered once at launch, before any view renders.
    static func registerFonts() {
        guard
            let url = Bundle.module.url(
                forResource: "DepartureMono-Regular", withExtension: "otf")
        else { return }
        CTFontManagerRegisterFontsForURL(url as CFURL, .process, nil)
    }
}

extension Font {
    /// The pixel voice. Values, transport labels, the wordmark.
    static func pixel(_ size: CGFloat) -> Font {
        .custom("Departure Mono", size: size)
    }
}

import UserNotifications

/// Posts the few notifications Foldover sends: outcomes of automatic syncs
/// that need a person (PLAN.md §3.8, "failures and conflicts only").
@MainActor
public final class UserAlerts {
    private var asked = false

    public init() {}

    /// Ask once; a refusal just means no notifications.
    public func post(identifier: String, title: String, body: String) async {
        let center = UNUserNotificationCenter.current()
        if !asked {
            asked = true
            _ = try? await center.requestAuthorization(options: [.alert, .sound])
        }
        let content = UNMutableNotificationContent()
        content.title = title
        content.body = body
        // The same identifier replaces an older alert for the same pair.
        try? await center.add(UNNotificationRequest(identifier: identifier, content: content, trigger: nil))
    }
}

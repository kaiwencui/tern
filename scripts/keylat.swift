// Measures how long a key press takes to reach the screen in any terminal:
// it presses keys as a keyboard would, watches the terminal's window, and
// times each press to the frame that first shows a change.
//
//   keylat PID COUNT OUT LABEL
//   keylat --check OUT
//
// PID is the terminal's process, whose window has to be in front and running
// something that echoes what is typed and shows nothing else (scripts/bench.sh
// runs `cat` with the cursor hidden). COUNT presses are made, a fifth to a
// third of a second apart at random, so that they fall anywhere in the
// screen's refresh. One line is appended to OUT for each:
//
//   {"label":"LABEL","event":"key","ms":12.3}
//
// The time runs from the moment the key event is handed to the system to the
// moment the system says the frame with the change was displayed, so it is in
// steps of the screen's refresh (16.7 ms at 60 Hz) and its average is what a
// press costs. It leaves out what a real keyboard adds before the system
// sees the key, which is the same for every terminal.
//
// macOS lets a program do neither thing unasked: it needs Screen Recording
// and Accessibility, under Privacy & Security in System Settings. Run without
// them, it says so in OUT and exits with status 3; with --check it only
// writes to OUT whether it has them ("screen" and "keys", true or false). bench.sh runs it through an
// app of its own (scripts/keylat-launcher.c), so that the permission is given
// to that app and not to the terminal the script happens to be run from.

import AppKit
import CoreGraphics
import CoreMedia
import Foundation
import ScreenCaptureKit

let arguments = CommandLine.arguments
if arguments.count == 3, arguments[1] == "--check" {
    let line = "{\"event\":\"permission\",\"screen\":\(CGPreflightScreenCaptureAccess()),\"keys\":\(CGPreflightPostEventAccess())}\n"
    FileManager.default.createFile(atPath: arguments[2], contents: Data(line.utf8))
    exit(0)
}
guard arguments.count == 5, let pid = Int32(arguments[1]), let count = Int(arguments[2]) else {
    FileHandle.standardError.write(Data("usage: keylat PID COUNT OUT LABEL\n".utf8))
    exit(2)
}
let outPath = arguments[3]
let label = arguments[4]

/// Says what went wrong, on standard error and in the result file (run from
/// an app, it has no terminal to say it to), and stops.
func fail(_ message: String, status: Int32 = 2) -> Never {
    FileHandle.standardError.write(Data("keylat: \(message)\n".utf8))
    let said = message.replacingOccurrences(of: "\\", with: "/").replacingOccurrences(of: "\"", with: "'")
    emit("{\"label\":\"\(label)\",\"event\":\"error\",\"message\":\"\(said)\"}")
    exit(status)
}

/// Appends a line to the result file.
func emit(_ line: String) {
    let data = Data((line + "\n").utf8)
    if let handle = FileHandle(forWritingAtPath: outPath) {
        handle.seekToEndOfFile()
        handle.write(data)
        handle.closeFile()
    } else {
        FileManager.default.createFile(atPath: outPath, contents: data)
    }
}

let mayWatch = CGPreflightScreenCaptureAccess()
let mayType = CGPreflightPostEventAccess()
if !mayWatch || !mayType {
    fail("needs Screen Recording (\(mayWatch ? "given" : "missing")) and Accessibility (\(mayType ? "given" : "missing")) in System Settings, Privacy & Security", status: 3)
}

var timebase = mach_timebase_info_data_t()
mach_timebase_info(&timebase)
func milliseconds(_ ticks: UInt64) -> Double {
    Double(ticks) * Double(timebase.numer) / Double(timebase.denom) / 1e6
}

/// The frames that showed a change, by when the system displayed them.
final class Frames: NSObject, SCStreamOutput, SCStreamDelegate {
    private let lock = NSLock()
    private var displayed: [UInt64] = []
    private let arrived = DispatchSemaphore(value: 0)
    var stopped: Error?

    func stream(_ stream: SCStream, didOutputSampleBuffer buffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen,
              let attachments = CMSampleBufferGetSampleAttachmentsArray(buffer, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
              let info = attachments.first,
              let raw = info[.status] as? Int,
              SCFrameStatus(rawValue: raw) == .complete,
              let time = info[.displayTime] as? UInt64
        else { return }
        lock.lock()
        displayed.append(time)
        lock.unlock()
        arrived.signal()
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        stopped = error
        arrived.signal()
    }

    /// Forgets the frames so far.
    func drain() {
        lock.lock()
        displayed.removeAll()
        lock.unlock()
        while arrived.wait(timeout: .now()) == .success {}
    }

    /// The display time of the first frame displayed after `after`, waiting
    /// up to `timeout` seconds for one.
    func first(after: UInt64, timeout: Double) -> UInt64? {
        let deadline = DispatchTime.now() + timeout
        while true {
            lock.lock()
            let found = displayed.first { $0 > after }
            lock.unlock()
            if let found { return found }
            if stopped != nil || arrived.wait(timeout: deadline) == .timedOut { return nil }
        }
    }
}

/// A box for what an asynchronous call gives back.
final class Outcome<T>: @unchecked Sendable {
    var value: T?
    var error: Error?
}

/// Waits for an asynchronous call and returns what it gave, or `nil` and the
/// error in `outcome`.
func waitFor<T>(_ work: @escaping @Sendable () async throws -> T) -> Outcome<T> {
    let done = DispatchSemaphore(value: 0)
    let outcome = Outcome<T>()
    Task.detached {
        do { outcome.value = try await work() } catch { outcome.error = error }
        done.signal()
    }
    done.wait()
    return outcome
}

/// The terminal's window: the largest one of the process that is on screen.
func terminalWindow() -> SCWindow {
    let listed = waitFor { try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true) }
    guard let content = listed.value else {
        fail("the windows on screen cannot be listed: \(listed.error?.localizedDescription ?? "no reason given")")
    }
    var largest: SCWindow?
    for window in content.windows {
        guard window.owningApplication?.processID == pid else { continue }
        let size = window.frame.size
        guard size.width > 100, size.height > 50 else { continue }
        let area = size.width * size.height
        if largest == nil || area > largest!.frame.width * largest!.frame.height {
            largest = window
        }
    }
    guard let window = largest else { fail("process \(pid) has no window on screen") }
    return window
}

/// Presses and lets go of a key, as the keyboard would, and returns the
/// moment the press was handed over.
func press(_ key: CGKeyCode) -> UInt64 {
    let source = CGEventSource(stateID: .hidSystemState)
    let down = CGEvent(keyboardEventSource: source, virtualKey: key, keyDown: true)
    let up = CGEvent(keyboardEventSource: source, virtualKey: key, keyDown: false)
    let at = mach_absolute_time()
    down?.post(tap: .cghidEventTap)
    up?.post(tap: .cghidEventTap)
    return at
}

/// Watches the window, presses the keys and writes down how long each took.
func measure() {
    // The keys go to whichever app is in front.
    NSRunningApplication(processIdentifier: pid)?.activate()
    Thread.sleep(forTimeInterval: 0.5)
    let window = terminalWindow()
    let scale = NSScreen.main?.backingScaleFactor ?? 1
    let configuration = SCStreamConfiguration()
    configuration.width = Int(window.frame.width * scale)
    configuration.height = Int(window.frame.height * scale)
    // As often as the screen shows a new frame.
    configuration.minimumFrameInterval = CMTime(value: 1, timescale: 240)
    configuration.queueDepth = 8
    configuration.showsCursor = false
    configuration.pixelFormat = kCVPixelFormatType_32BGRA

    let frames = Frames()
    let filter = SCContentFilter(desktopIndependentWindow: window)
    let stream = SCStream(filter: filter, configuration: configuration, delegate: frames)
    do {
        try stream.addStreamOutput(frames, type: .screen, sampleHandlerQueue: DispatchQueue(label: "keylat.frames"))
    } catch {
        fail("the window cannot be watched: \(error.localizedDescription)")
    }
    let started = waitFor { try await stream.startCapture() }
    if let error = started.error {
        fail("the window cannot be watched: \(error.localizedDescription)")
    }

    // The first frames after a stream starts are the window as it is.
    Thread.sleep(forTimeInterval: 1.0)

    // The keys a, s, d and f, and Return.
    let letters: [CGKeyCode] = [0, 1, 2, 3]
    let returnKey: CGKeyCode = 36
    var generator = SystemRandomNumberGenerator()
    var missed = 0
    for index in 0..<count {
        // A line ended before it is as long as the terminal's line editor
        // takes; what is echoed back for it is waited out.
        if index > 0 && index % 40 == 0 {
            _ = press(returnKey)
            Thread.sleep(forTimeInterval: 0.5)
        }
        Thread.sleep(forTimeInterval: Double.random(in: 0.2...0.33, using: &generator))
        frames.drain()
        let at = press(letters[index % letters.count])
        if let displayed = frames.first(after: at, timeout: 1.0) {
            emit(String(format: "{\"label\":\"%@\",\"event\":\"key\",\"ms\":%.2f}", label, milliseconds(displayed - at)))
        } else {
            missed += 1
        }
        if let error = frames.stopped {
            fail("the window can no longer be watched: \(error.localizedDescription)")
        }
    }
    emit("{\"label\":\"\(label)\",\"event\":\"keys\",\"pressed\":\(count),\"missed\":\(missed)}")
    _ = waitFor { try await stream.stopCapture() }
}

measure()

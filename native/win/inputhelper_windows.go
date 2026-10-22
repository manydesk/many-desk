// ManyDesk input helper for Windows.
// Same newline-delimited JSON protocol as native/InputHelper.swift, except:
//   - x/y are physical screen pixels (virtual desktop coordinates)
//   - key commands carry a PS/2 set-1 scan code "k" and extended flag "e"
//   - {"t":"text","s":"..."} types text as Unicode (layout independent)
//   - {"t":"block","on":true} privacy mode: BlockInput (needs an elevated or SYSTEM helper)
// Before every command the thread follows the input desktop, so that when the helper runs
// as SYSTEM (ManyDesk service) it also reaches the sign-in screen, UAC and the lock screen.
//
// Built from macOS with: GOOS=windows GOARCH=amd64 go build
package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"runtime"
	"syscall"
	"unicode/utf16"
	"unsafe"
)

var (
	user32                         = syscall.NewLazyDLL("user32.dll")
	procSendInput                  = user32.NewProc("SendInput")
	procSetCursorPos               = user32.NewProc("SetCursorPos")
	procSetProcessDpiAwarenessCtx  = user32.NewProc("SetProcessDpiAwarenessContext")
	procSetProcessDPIAware         = user32.NewProc("SetProcessDPIAware")
	procBlockInput                 = user32.NewProc("BlockInput")
	procOpenInputDesktop           = user32.NewProc("OpenInputDesktop")
	procSetThreadDesktop           = user32.NewProc("SetThreadDesktop")
	procCloseDesktop               = user32.NewProc("CloseDesktop")
	procGetUserObjectInformationW  = user32.NewProc("GetUserObjectInformationW")
)

var currentDesk string
var currentHandle uintptr

// followInputDesktop attaches this thread to the desktop that receives input now
// (Default, Winlogon, or the UAC secure desktop). Best effort: a non-SYSTEM helper
// simply stays on its own desktop.
func followInputDesktop() {
	const desktopAll = 0x01FF // DESKTOP_* rights
	h, _, _ := procOpenInputDesktop.Call(0, 0, desktopAll)
	if h == 0 {
		return
	}
	var buf [256]uint16
	var need uint32
	procGetUserObjectInformationW.Call(h, 2 /* UOI_NAME */, uintptr(unsafe.Pointer(&buf[0])), uintptr(len(buf)*2), uintptr(unsafe.Pointer(&need)))
	name := syscall.UTF16ToString(buf[:])
	if name == currentDesk {
		procCloseDesktop.Call(h)
		return
	}
	if r, _, _ := procSetThreadDesktop.Call(h); r == 0 {
		procCloseDesktop.Call(h)
		return
	}
	if currentHandle != 0 {
		procCloseDesktop.Call(currentHandle)
	}
	currentDesk, currentHandle = name, h
}

const (
	inputMouse    = 0
	inputKeyboard = 1

	mouseLeftDown   = 0x0002
	mouseLeftUp     = 0x0004
	mouseRightDown  = 0x0008
	mouseRightUp    = 0x0010
	mouseMiddleDown = 0x0020
	mouseMiddleUp   = 0x0040
	mouseWheel      = 0x0800
	mouseHWheel     = 0x1000

	keyExtended = 0x0001
	keyUp       = 0x0002
	keyUnicode  = 0x0004
	keyScancode = 0x0008
)

// INPUT layouts for 64-bit Windows (sizeof(INPUT) == 40).
type mouseInput struct {
	typ       uint32
	_         uint32
	dx, dy    int32
	mouseData uint32
	flags     uint32
	time      uint32
	_         uint32
	extra     uintptr
}

type keyInput struct {
	typ   uint32
	_     uint32
	vk    uint16
	scan  uint16
	flags uint32
	time  uint32
	_     uint32
	extra uintptr
	_     [8]byte
}

type command struct {
	T  string  `json:"t"`
	X  float64 `json:"x"`
	Y  float64 `json:"y"`
	B  int     `json:"b"`
	Dx float64 `json:"dx"`
	Dy float64 `json:"dy"`
	K  uint16  `json:"k"`
	E  bool    `json:"e"`
	D  bool    `json:"d"`
	S  string  `json:"s"`
	On bool    `json:"on"`
}

type heldKey struct {
	scan uint16
	ext  bool
}

var (
	pressedButtons = map[int]bool{}
	pressedKeys    = map[heldKey]bool{}
)

func sendMouse(flags uint32, data int32) {
	in := mouseInput{typ: inputMouse, mouseData: uint32(data), flags: flags}
	procSendInput.Call(1, uintptr(unsafe.Pointer(&in)), unsafe.Sizeof(in))
}

func sendKey(scan uint16, ext, down bool) {
	flags := uint32(keyScancode)
	if ext {
		flags |= keyExtended
	}
	if !down {
		flags |= keyUp
	}
	in := keyInput{typ: inputKeyboard, scan: scan, flags: flags}
	procSendInput.Call(1, uintptr(unsafe.Pointer(&in)), unsafe.Sizeof(in))
}

func typeText(s string) {
	for _, u := range utf16.Encode([]rune(s)) {
		for _, up := range []bool{false, true} {
			flags := uint32(keyUnicode)
			if up {
				flags |= keyUp
			}
			in := keyInput{typ: inputKeyboard, scan: u, flags: flags}
			procSendInput.Call(1, uintptr(unsafe.Pointer(&in)), unsafe.Sizeof(in))
		}
	}
}

func moveTo(x, y float64) {
	procSetCursorPos.Call(uintptr(int32(x)), uintptr(int32(y)))
}

func buttonFlags(b int) (down, up uint32) {
	switch b {
	case 2:
		return mouseRightDown, mouseRightUp
	case 1:
		return mouseMiddleDown, mouseMiddleUp
	default:
		return mouseLeftDown, mouseLeftUp
	}
}

func handle(c command) {
	followInputDesktop()
	switch c.T {
	case "text":
		typeText(c.S)
	case "block":
		on := uintptr(0)
		if c.On {
			on = 1
		}
		r, _, _ := procBlockInput.Call(on)
		fmt.Printf("{\"t\":\"block\",\"on\":%v}\n", c.On && r != 0)
	case "move":
		moveTo(c.X, c.Y)
	case "down", "up":
		moveTo(c.X, c.Y)
		down, up := buttonFlags(c.B)
		if c.T == "down" {
			pressedButtons[c.B] = true
			sendMouse(down, 0)
		} else {
			delete(pressedButtons, c.B)
			sendMouse(up, 0)
		}
	case "scroll":
		// Browser pixel deltas: ~100px per notch; Windows WHEEL_DELTA is 120 per notch.
		if c.Dy != 0 {
			sendMouse(mouseWheel, int32(-c.Dy*1.2))
		}
		if c.Dx != 0 {
			sendMouse(mouseHWheel, int32(c.Dx*1.2))
		}
	case "key":
		k := heldKey{c.K, c.E}
		if c.D {
			pressedKeys[k] = true
		} else {
			delete(pressedKeys, k)
		}
		sendKey(c.K, c.E, c.D)
	case "release":
		for k := range pressedKeys {
			sendKey(k.scan, k.ext, false)
		}
		pressedKeys = map[heldKey]bool{}
		for b := range pressedButtons {
			_, up := buttonFlags(b)
			sendMouse(up, 0)
		}
		pressedButtons = map[int]bool{}
	case "ping":
		fmt.Println(`{"t":"pong","trusted":true}`)
	}
}

func main() {
	// SetThreadDesktop works per OS thread: keep all commands on this one.
	runtime.LockOSThread()
	// Per-monitor DPI awareness so SetCursorPos works in physical pixels on scaled displays.
	if procSetProcessDpiAwarenessCtx.Find() == nil {
		procSetProcessDpiAwarenessCtx.Call(^uintptr(3)) // DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2 (-4)
	} else {
		procSetProcessDPIAware.Call()
	}
	fmt.Println(`{"t":"ready","trusted":true}`)

	sc := bufio.NewScanner(os.Stdin)
	sc.Buffer(make([]byte, 64*1024), 1024*1024)
	for sc.Scan() {
		var c command
		if json.Unmarshal(sc.Bytes(), &c) == nil {
			handle(c)
		}
	}
}

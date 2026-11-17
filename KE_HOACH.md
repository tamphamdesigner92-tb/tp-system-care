# Kế hoạch TP System Care

Ứng dụng chăm sóc và tối ưu Windows, phong cách giao diện tương tự Advanced SystemCare (nền tối, thanh bên trái, nút Quét tròn lớn, công tắc Chế độ AI / Thủ công) nhưng dùng tên, logo, icon và nội dung riêng.

_Cập nhật lần cuối: 18/11/2026_

## Tóm tắt tiến độ

| Giai đoạn | Nội dung | Ước lượng | Trạng thái |
|---|---|---|---|
| 1 | Khung ứng dụng, giao diện chính, điều hướng, đa ngôn ngữ, quyền admin | 1–2 tuần | ✅ Hoàn thành (16/11/2026) |
| 2 | Engine quét, Rescue Center, 4 module an toàn | 2–3 tuần | ✅ Hoàn thành (16/11/2026), còn việc thử tay |
| 3 | Registry, tối ưu hệ thống/Internet/ổ đĩa, Chế độ AI, dọn sâu, điểm sức khỏe | 4–5 tuần | ✅ Hoàn thành (16/11/2026), còn việc thử tay |
| 4 | Tăng tốc, Dọn dẹp tùy chỉnh, tinh chỉnh trình duyệt, Gỡ ứng dụng sạch | 5–7 tuần | Đang làm (17/11/2026) |
| 5 | Bảo vệ (thời gian thực, Safe Folder, email, Anti-Tracking), Khóa thư mục, Cập nhật phần mềm và driver, Hộp công cụ | 10–14 tuần | Chưa bắt đầu |
| 6 | Service nền, Auto Care, tự giải phóng RAM, CPU Saver, widget, bộ cài, tự cập nhật, kiểm thử | 3–5 tuần | Chưa bắt đầu |

Tổng ước lượng: 7–10 tháng cho một người làm toàn thời gian, đã gồm các tính năng bản Pro (thêm 5–7 tuần), Gỡ ứng dụng sạch (thêm 2–3 tuần), Cập nhật driver (thêm 2–3 tuần) và Khóa thư mục (thêm 2–3 tuần). Rust dài hơn C# khoảng 20–30% do gọi API Windows dài dòng hơn.

## Quyết định đã chốt

- **Ngôn ngữ:** Rust cho lõi và service.
- **Giao diện:** Tauri 2, frontend Svelte 5 + TypeScript + Vite.
- **Phạm vi:** không tự xây diệt virus thời gian thực và kho driver riêng; thay bằng tích hợp Windows Defender, `winget` và danh mục driver của Windows Update.
- **Bản quyền:** không dùng tên, logo, tài nguyên của IObit.
- **Khóa thư mục:** có cả hai mức Khóa nhanh (quyền NTFS) và Khóa mã hóa (AES-256), mặc định Khóa nhanh; lấy lại mật khẩu bằng mã khôi phục, không qua email.
- **Quyền admin:** bản release yêu cầu UAC khi mở; bản dev chạy quyền thường để `tauri dev` chạy được từ terminal thường.

## Kiến trúc

| Thành phần | Vai trò | Thư viện chính | Trạng thái |
|---|---|---|---|
| `crates/tp-core` | Engine quét/sửa, Rescue Center, công cụ hệ thống | `windows`, `winreg`, `walkdir` (sau này thêm `wmi`, `sysinfo`) | Có engine, Rescue Center, 10 module, luật Chế độ AI, điểm sức khỏe; công cụ Tăng tốc, Gỡ sạch và Tìm tệp lớn |
| `app/src-tauri` | Ứng dụng Tauri: cửa sổ, lệnh gọi từ giao diện, tray | `tauri` 2 | Đã tạo |
| `app/src` | Giao diện Svelte | `svelte` 5, `@tauri-apps/api` | Đã tạo |
| `crates/tp-service` | Windows Service quyền admin, lịch tự động, giao tiếp qua named pipe | `windows-service` | Chưa tạo (giai đoạn 6) |

Mỗi module quét dùng chung một interface (`engine::Module`): `scan()` trả về danh sách mục (`Finding`), mỗi mục mang một hành động (`Action`); `engine::fix()` thực thi hành động và ghi vào Rescue Center. Module chỉ đọc, mọi thay đổi đi qua `fix()`.

Quy tắc hoàn tác: thay đổi cấu hình (lối tắt, mục khởi động, và registry từ giai đoạn 3) phải sao lưu và hoàn tác được. Tệp rác và dấu vết riêng tư bị xóa hẳn theo đúng mục đích, chỉ được ghi vào lịch sử.

## Tính năng theo từng mục thanh bên

1. **Chăm sóc (màn hình Quét)**
   - Dọn rác, dấu vết riêng tư, lối tắt hỏng, registry.
   - Tối ưu khởi động, hệ thống, Internet, ổ đĩa.
   - Vá lỗ hổng (bản vá Windows), gia cố bảo mật.
   - Chế độ AI: tự chọn hạng mục theo tình trạng máy và lịch sử quét (dựa trên luật). Chế độ thủ công: người dùng tự chọn.
2. **Dọn dẹp tùy chỉnh:** rác theo ứng dụng và trình duyệt, tệp lớn, tệp trùng, dọn Windows Update và WinSxS.
3. **Tăng tốc:** Turbo Boost, quản lý khởi động, giải phóng RAM, tối ưu sâu, gỡ ứng dụng và toolbar.
4. **Bảo vệ:** khóa thư mục bằng mật khẩu, trạng thái Defender và Firewall, chống theo dõi trình duyệt, bảo vệ trang chủ và DNS, chặn truy cập webcam/micro.
5. **Cập nhật phần mềm:** quét ứng dụng đã cài, so phiên bản, cập nhật qua `winget`.
6. **Hộp công cụ:** gỡ cài đặt sạch, hủy tệp, khôi phục tệp đã xóa, kiểm tra ổ đĩa, quản lý tiến trình, sửa lỗi Windows, thông tin phần cứng.

## Tính năng bản Pro bổ sung

Nguồn: bảng so sánh Free/Pro trên trang Advanced SystemCare 20 PRO của IObit (tra ngày 16/11/2026). TP System Care không chia bản Free/Pro: mọi tính năng dưới đây mở cho tất cả người dùng, không có hệ thống bản quyền.

| Tính năng Pro | Cách làm trong TP System Care | Giai đoạn | Mức khả thi |
|---|---|---|---|
| Dọn sâu rác ẩn (Disk Deep Clean) | Mở rộng module rác: cache ứng dụng, tệp tạm, tàn dư sau gỡ cài đặt, tệp log, bản dump | 3 | Tự làm được |
| Dọn registry (Pro mới có) | Đã có trong kế hoạch gốc | 3 | Tự làm được, rủi ro cao |
| Tăng tốc PC "đến 2×" / tối ưu sâu | Tinh chỉnh service, power plan, hiệu ứng hình ảnh, tác vụ nền | 3–4 | Tự làm được; không cam kết con số |
| Tự giải phóng RAM | Service theo dõi RAM, tự dọn khi vượt ngưỡng người dùng đặt | 6 | Tự làm được |
| CPU Saver | Hạ độ ưu tiên hoặc tạm dừng tiến trình nền khi CPU quá tải; có danh sách loại trừ | 6 | Tự làm được |
| Tinh chỉnh trình duyệt "nhanh hơn 3×" | Dọn cache, tắt tiện ích nặng, tối ưu DNS và thiết lập mạng | 4 | Một phần; không cam kết con số |
| Kiểm tra sức khỏe PC và tự sửa (Fix Issues) | Điểm sức khỏe tổng hợp từ các module, nút sửa một lần | 3 | Tự làm được |
| Tự động tối ưu / bảo trì thông minh (Auto Care) | Lịch quét và sửa tự động khi máy rảnh, chạy trong `tp-service` | 6 | Tự làm được |
| Bảo vệ PC thời gian thực | Bật, theo dõi và điều khiển Windows Defender; không tự viết engine diệt virus | 5 | Qua Defender |
| Bảo vệ email (Gmail, Outlook, Yahoo) | Chặn liên kết lừa đảo bằng danh sách chặn ở tầng DNS/hosts | 5 | Một phần |
| Safe Folder (chống ransomware) | Dùng Controlled Folder Access của Windows; tự chặn ở mức tệp cần driver nhân có chữ ký | 5 | Qua Windows |
| Chặn chương trình lạ truy cập dữ liệu | Cùng cơ chế Controlled Folder Access, kèm danh sách ứng dụng tin cậy | 5 | Qua Windows |
| Tự xóa dấu vết trực tuyến (Anti-Tracking) | Tự dọn lịch sử, cookie khi đóng trình duyệt | 5 | Tự làm được |
| Che danh tính trực tuyến (Identity Masking) | Chống lấy dấu vân tay trình duyệt bằng thiết lập chính sách trình duyệt | 5 | Một phần |
| Cập nhật phần mềm một lần bấm | `winget upgrade` cho tất cả, kèm tự cập nhật theo lịch | 5 | Tự làm được |
| Hỗ trợ kỹ thuật cao cấp | Không áp dụng | — | Bỏ |

Các mục ghi "không cam kết con số" là khẩu hiệu quảng cáo của IObit; kế hoạch chỉ làm phần kỹ thuật đứng sau chúng.

## Cập nhật driver (tương tự Driver Booster)

Nằm trong trang "Cập nhật phần mềm", thành tab thứ hai: Phần mềm / Driver. Làm ở giai đoạn 5.

Nguồn driver là danh mục Windows Update của Microsoft (qua Windows Update Agent API), không phải kho riêng. Driver ở đây đều đã qua kiểm định WHQL nên an toàn, nhưng thường cũ hơn bản trên trang hãng và ít hơn kho của Driver Booster. Driver card đồ họa (NVIDIA, AMD, Intel) sẽ dẫn người dùng sang trang tải chính thức của hãng thay vì tự cài.

| Chức năng | Cách làm |
|---|---|
| Quét thiết bị và driver hiện tại | SetupAPI / WMI `Win32_PnPSignedDriver`: tên, hãng, phiên bản, ngày, trạng thái lỗi |
| Tìm bản mới | Windows Update Agent: tìm cập nhật loại Driver chưa cài |
| Phát hiện thiết bị thiếu driver hoặc lỗi | Mã lỗi Device Manager (thiết bị có dấu chấm than) |
| Sao lưu driver trước khi cập nhật | `pnputil /export-driver` vào thư mục Rescue Center |
| Tạo điểm khôi phục trước khi cài | Dùng chung Rescue Center |
| Cài đặt, một cái hoặc tất cả | Windows Update Agent tải và cài; báo khi cần khởi động lại |
| Khôi phục driver cũ (rollback) | Cài lại từ bản sao lưu bằng `pnputil /add-driver` |
| Danh sách bỏ qua | Người dùng loại trừ thiết bị không muốn cập nhật |
| Tự quét theo lịch | Qua `tp-service` (giai đoạn 6); chỉ thông báo, không tự cài |

## Gỡ ứng dụng sạch (tương tự IObit Uninstaller)

Là một công cụ đầy đủ trong "Hộp công cụ", có lối tắt từ trang "Tăng tốc". Làm ở giai đoạn 4, thay cho mục "gỡ ứng dụng và toolbar" sơ sài trước đây.

| Chức năng | Cách làm |
|---|---|
| Liệt kê ứng dụng đã cài | Khóa registry Uninstall (HKLM, HKCU, 32/64-bit), MSI, ứng dụng Store/UWP |
| Thông tin và sắp xếp | Kích thước, ngày cài, lần dùng gần nhất; lọc ứng dụng lớn, ít dùng, mới cài |
| Gỡ chuẩn rồi quét tàn dư | Chạy trình gỡ gốc, sau đó quét tệp, thư mục, registry, service, tác vụ định kỳ, lối tắt, mục khởi động còn sót |
| Xem trước tàn dư | Hiện danh sách để người dùng chọn trước khi xóa; không tự xóa mục không chắc chắn |
| Gỡ cưỡng bức | Cho ứng dụng hỏng trình gỡ hoặc không có trong danh sách: xóa theo thư mục cài và dấu vết liên quan |
| Gỡ hàng loạt | Chọn nhiều ứng dụng, gỡ lần lượt, ưu tiên chế độ im lặng nếu trình gỡ hỗ trợ |
| Gỡ ứng dụng Windows cài sẵn | `Remove-AppxPackage` cho ứng dụng Store và bloatware |
| Gỡ tiện ích và toolbar trình duyệt | Chrome, Edge, Firefox: liệt kê và xóa tiện ích |
| Gỡ bản cập nhật Windows | Liệt kê và gỡ bản cập nhật gây lỗi |
| Theo dõi cài đặt (Install Monitor) | Chụp trước/sau trong thư mục cài đã chọn và các nhánh registry giới hạn; lưu nhật ký để đối chiếu khi gỡ. Thay đổi đồng thời không chứng minh thuộc bộ cài, không bảo đảm sạch tuyệt đối |
| Phát hiện phần mềm đi kèm | Cảnh báo ứng dụng được cài cùng lúc mà người dùng không chủ ý |
| An toàn | Điểm khôi phục và sao lưu registry trước mỗi lần gỡ; tệp tàn dư vào Thùng rác, có tùy chọn hủy hẳn |

## Khóa thư mục bằng mật khẩu

Yêu cầu thêm ngày 16/11/2026: người dùng chọn thư mục để khóa, mở bằng mật khẩu, đổi được mật khẩu và lấy lại được khi quên. Nằm trong trang "Bảo vệ", có lối tắt từ "Hộp công cụ". Làm ở giai đoạn 5.

Có hai cách khóa, khác nhau về mức bảo vệ. Đã chốt ngày 16/11/2026: làm cả hai, mặc định là Khóa nhanh, và ghi rõ mức bảo vệ ngay trên giao diện.

| Mức | Cách làm | Ưu điểm | Giới hạn |
|---|---|---|---|
| Khóa nhanh | Đặt quyền NTFS từ chối truy cập cho mọi tài khoản, kèm thuộc tính ẩn và hệ thống | Tức thì với thư mục lớn bất kỳ, không đụng tới nội dung tệp | Không mã hóa: người có quyền quản trị và hiểu biết vẫn gỡ được quyền; gắn ổ sang máy khác là đọc được. Chỉ chạy trên ổ NTFS |
| Khóa mã hóa | Mã hóa từng tệp bằng AES-256-GCM, tên tệp cũng được mã hóa | Bảo vệ thật kể cả khi tháo ổ cứng | Chậm với thư mục lớn (tỉ lệ với dung lượng), cần đủ chỗ trống tạm; mất cả mật khẩu lẫn mã khôi phục là mất dữ liệu |

| Chức năng | Cách làm |
|---|---|
| Chọn thư mục để khóa | Hộp chọn thư mục, kéo thả vào danh sách, và mục "Khóa bằng TP System Care" trong menu chuột phải của Explorer |
| Thư mục không cho khóa | Thư mục hệ thống (`Windows`, `Program Files`, `ProgramData`), thư mục hồ sơ người dùng ở cấp gốc, gốc ổ đĩa, thư mục cài của chính ứng dụng |
| Mật khẩu chung | Một mật khẩu chính cho mọi thư mục đã khóa. Không lưu mật khẩu: chỉ lưu khóa dẫn xuất bằng Argon2id kèm salt |
| Mở khóa | Nhập mật khẩu để mở tạm một thư mục hoặc gỡ khóa hẳn; thư mục mở tạm được khóa lại khi đăng xuất hoặc sau thời gian đặt trước |
| Đổi mật khẩu | Nhập mật khẩu cũ và mới. Khóa mã hóa dữ liệu được bọc bằng mật khẩu nên chỉ cần bọc lại, không phải mã hóa lại tệp |
| Lấy lại khi quên | Mã khôi phục gồm 24 ký tự tạo lúc đặt mật khẩu lần đầu, bắt người dùng lưu hoặc in ra. Mã này bọc cùng một khóa dữ liệu nên dùng nó đặt lại được mật khẩu. Kèm gợi ý mật khẩu do người dùng tự ghi |
| Chống dò mật khẩu | Thời gian chờ tăng dần sau mỗi lần nhập sai |
| Danh sách thư mục đã khóa | Trạng thái, mức khóa, ngày khóa; mở, khóa lại, gỡ khỏi danh sách |
| An toàn dữ liệu khi mã hóa | Mã hóa ra tệp mới rồi mới xóa tệp gốc; có nhật ký để chạy tiếp nếu bị ngắt giữa chừng (mất điện, tắt máy) |
| Gỡ ứng dụng | Trình gỡ cảnh báo và yêu cầu mở khóa hết trước khi gỡ, để không bỏ lại thư mục không mở được |

Không làm khôi phục qua email: ứng dụng không có máy chủ và tài khoản người dùng. Nếu sau này có, có thể thêm.

## Chi tiết từng giai đoạn

### Giai đoạn 1 — Khung ứng dụng ✅

Đã làm:

- [x] Cài môi trường: Rust 1.100.0 (rustup qua winget), Node 24, WebView2 có sẵn, Visual Studio 2026 (MSVC).
- [x] Cargo workspace (`Cargo.toml`) gồm `crates/tp-core` và `app/src-tauri`; profile release tối ưu kích thước.
- [x] Cửa sổ không viền 980×560 với thanh tiêu đề tự vẽ: nút đổi ngôn ngữ, thu nhỏ, phóng to, đóng, vùng kéo cửa sổ.
- [x] Thanh bên 6 mục, tên ứng dụng, dòng trạng thái quyền quản trị.
- [x] Màn hình Chăm sóc: nút Quét tròn có vòng xoay; Chế độ AI; Chế độ thủ công với 10 hạng mục và "Chọn tất cả".
- [x] Nút Quét hiện thông báo "Engine quét chưa có" (không giả lập kết quả).
- [x] 5 trang còn lại là trang giữ chỗ, ghi "Chưa xây dựng" kèm mô tả.
- [x] Đa ngôn ngữ Việt/Anh, nhớ lựa chọn.
- [x] Lệnh Rust `is_elevated` kiểm tra quyền admin qua token tiến trình.
- [x] Manifest `requireAdministrator` chỉ nhúng vào bản release.
- [x] Bộ icon riêng (vòng tròn xanh, chữ "TP") — bản tạm.
- [x] Build: `svelte-check` 0 lỗi; `cargo build` không lỗi; bản release `tp-system-care.exe` 3,0 MB; bộ cài NSIS 1,12 MB.

Đã kiểm tra:

- Giao diện trong trình duyệt: chuyển chế độ, nút Quét, đổi ngôn ngữ, chuyển trang, không lỗi console.
- Cửa sổ thật (bản dev): hiển thị đúng, trạng thái quyền quản trị báo "Không".
- Bản release có nhúng `requireAdministrator`, bản dev thì không.

Còn tồn đọng:

- [ ] Bấm thử nút thu nhỏ / phóng to / đóng và kéo cửa sổ trên ứng dụng thật.
- [ ] Chạy bản release và bộ cài, xác nhận UAC và trạng thái "Quyền quản trị: Có".
- [ ] Bộ cài chưa ký số, SmartScreen có thể cảnh báo (xử lý ở giai đoạn 6).
- [ ] Icon tạm, cần thiết kế lại trước khi phát hành.

### Giai đoạn 2 — Engine quét và module an toàn ✅

Đã làm:

- [x] Interface chung cho module (`crates/tp-core/src/engine.rs`); tiến độ quét và sửa gửi lên giao diện qua sự kiện Tauri `scan-progress`, `fix-progress`; dừng giữa chừng được.
- [x] Rescue Center (`rescue.rs`): lịch sử trong `%LOCALAPPDATA%\TP System Care\Rescue\history.json`, cách ly tệp, xuất khóa registry ra `.reg`, hoàn tác từng lần sửa, tạo điểm khôi phục hệ thống (qua `Checkpoint-Computer`, cần quyền quản trị).
- [x] Module Dọn tệp rác: tệp tạm người dùng và Windows (chỉ tệp cũ hơn 24 giờ), bộ nhớ đệm Chrome/Edge/Brave/Firefox, ảnh thu nhỏ, báo cáo lỗi, bản dump, Thùng rác (không chọn sẵn).
- [x] Module Xóa dấu vết riêng tư: tệp mở gần đây, lịch sử hộp Run, đường dẫn gõ trong Explorer; lịch sử và cookie trình duyệt (không chọn sẵn). Không đụng Jump List và `places.sqlite` của Firefox.
- [x] Module Sửa lối tắt hỏng: Desktop và Start Menu (riêng và chung); chỉ tính là hỏng khi đích nằm trên ổ cứng cố định và chắc chắn không còn. Sửa bằng cách chuyển vào Rescue Center.
- [x] Module Tối ưu khởi động: khóa Run (HKCU, HKLM, 32-bit) và thư mục Startup; tắt bằng khóa `StartupApproved` như Task Manager, không xóa gì. Chỉ chọn sẵn mục trỏ tới tệp đã mất; bỏ qua thành phần của Windows.
- [x] Màn hình quét: tiến độ theo hạng mục, kết quả theo nhóm có ô chọn, nút Sửa, màn hình tổng kết; trang Rescue Center mở từ thanh tiêu đề.
- [x] Chế độ thủ công: 4 hạng mục đã có chọn được, 6 hạng mục còn lại ghi "Sắp có". Chế độ AI tạm quét cả 4 hạng mục (luật chọn thuộc giai đoạn 3).
- [x] Backend giả cho bản dev: mở `http://localhost:1420/?mock` để xem giao diện trong trình duyệt; không có trong bản phát hành.

Đã kiểm tra:

- 9 unit test của `tp-core` đạt (`cargo test -p tp-core`): xóa tệp chỉ trong thư mục cho phép, cách ly rồi hoàn tác, ghi và khôi phục giá trị registry trên khóa thử, tách đường dẫn từ dòng lệnh.
- Quét thật trên máy phát triển (chỉ đọc, `cargo run -p tp-core --example scan`): ra kết quả đúng ở cả 4 module, lối tắt báo hỏng đã được đối chiếu là hỏng thật.
- Giao diện với backend giả: quét, kết quả, sửa, tổng kết, Rescue Center và hoàn tác; không lỗi console. `svelte-check` 0 lỗi.
- Build release không lỗi: `tp-system-care.exe` 3,2 MB, bộ cài NSIS 1,19 MB; dữ liệu giả không có trong gói.

Còn tồn đọng:

- [ ] Chưa chạy Sửa trên dữ liệu thật qua cửa sổ ứng dụng (mới kiểm tra bằng unit test trên thư mục tạm và khóa registry thử).
- [ ] Chưa thử tạo điểm khôi phục và sửa các mục HKLM với quyền quản trị.
- [ ] Trình duyệt đang mở sẽ khóa tệp cache, lịch sử, cookie: các tệp đó được báo là "không xử lý được"; chưa có bước nhắc đóng trình duyệt.
- [ ] Quét tệp tạm lớn (hơn 10.000 tệp) mất khoảng 25 giây ở bản debug; chưa đo bản release.

### Giai đoạn 3 — Registry, tối ưu, Chế độ AI ✅

Đã làm:

- [x] Lớp registry dùng chung (`reg.rs`) và các hành động mới của engine: đặt giá trị, xóa giá trị, xóa khóa phẳng, bật/tắt thiết lập hệ thống, mở trang cài đặt Windows. Mọi thay đổi registry đều lưu giá trị cũ để hoàn tác chính xác và xuất khóa ra `.reg` trước khi xóa.
- [x] Dọn registry (`modules/registry.rs`), luật thận trọng: chỉ gỡ mục trỏ tới tệp chắc chắn đã mất ở 4 nơi (SharedDLLs, App Paths, MuiCache, Compatibility Assistant Store). Không đụng COM/CLSID, liên kết kiểu tệp, khóa Uninstall, dịch vụ.
- [x] Tối ưu hệ thống (`modules/tweaks.rs`): độ trễ menu, độ trễ chạy chương trình khởi động, chặn Windows tự cài ứng dụng quảng bá.
- [x] Tăng tốc Internet: phát hiện proxy hệ thống đang bật nhưng trỏ về cổng chết trên máy; bỏ giới hạn mạng khi phát đa phương tiện (không chọn sẵn).
- [x] Tối ưu ổ đĩa (`modules/disk.rs`): TRIM đang tắt, lịch tối ưu ổ đĩa đang tắt.
- [x] Vá lỗ hổng (`modules/vuln.rs`): hỏi Windows Update Agent các bản cập nhật chưa cài, đánh dấu bản vá bảo mật. Nút Sửa mở trang Windows Update; ứng dụng không tự tải và cài bản vá.
- [x] Gia cố bảo mật: UAC tắt, tường lửa tắt (3 hồ sơ mạng), SMBv1 bật; Remote Desktop, Remote Assistance và ẩn đuôi tệp (không chọn sẵn).
- [x] Chế độ AI chọn hạng mục theo luật (`state.rs`): chưa quét, còn vấn đề chưa sửa, quá hạn quét lại (1–14 ngày tùy hạng mục), ổ hệ thống còn dưới 10%. Lịch sử quét lưu ở `%LOCALAPPDATA%\TP System Care\state.json`.
- [x] (Pro) Dọn sâu, gộp vào module Dọn tệp rác: cache ứng dụng Electron, cache shader và Internet của Windows, log Windows cũ hơn 7 ngày, cache tải bản cập nhật, dump LiveKernelReports.
- [x] (Pro) Điểm sức khỏe 0–100 theo trọng số từng hạng mục, chỉ tính mục được khuyến nghị sửa; nút "Quét và sửa một lần" quét cả 10 hạng mục rồi tự sửa các mục khuyến nghị.

Đã kiểm tra:

- 26 unit test của `tp-core` đạt, gồm sửa rồi hoàn tác registry trên khóa thử (đặt giá trị, xóa giá trị, xóa khóa), luật Chế độ AI, cách tính điểm, phân tích đầu ra `fsutil`/`schtasks`/Windows Update.
- Quét thật cả 10 hạng mục trên máy phát triển (chỉ đọc, bản release): 43 giây, trong đó rác 16 giây và Windows Update 25 giây. Kết quả Windows Update (0 bản chưa cài) đã đối chiếu bằng truy vấn trực tiếp.
- Giao diện với backend giả: màn hình Chế độ AI, quét 10 hạng mục, "Quét và sửa một lần", điểm sức khỏe sau khi sửa; không lỗi console. `svelte-check` 0 lỗi.
- Build release không lỗi: `tp-system-care.exe` 3,3 MB, bộ cài NSIS 1,22 MB.

Còn tồn đọng:

- [ ] Chưa chạy Sửa trên dữ liệu thật qua cửa sổ ứng dụng cho các hạng mục mới; các mục HKLM, TRIM và lịch tối ưu ổ đĩa cần quyền quản trị nên chưa thử được ở bản dev.
- [ ] Máy phát triển không có vấn đề về ổ đĩa, bảo mật (trừ ẩn đuôi tệp) và bản vá, nên nhánh "có vấn đề" của các hạng mục đó mới được kiểm bằng unit test.
- [ ] Tra cứu Windows Update chậm (khoảng 25 giây) và cần mạng; quá 120 giây thì hạng mục bị bỏ qua mà chưa báo lý do lên giao diện.
- [ ] Một số thay đổi bảo mật (UAC, SMBv1) chỉ có hiệu lực sau khi khởi động lại; giao diện mới ghi chú chung, chưa nhắc khởi động lại.

### Giai đoạn 4 — Tăng tốc, Dọn dẹp tùy chỉnh — Đang làm

Đã làm:

- [x] Module công cụ riêng (`crates/tp-core/src/tools/`) và lệnh Tauri; giao diện chỉ gửi mã của mục backend đã liệt kê.
- [x] Tăng tốc: Turbo Boost đổi power plan sang Hiệu năng cao/Ultimate, lưu gói nguồn cũ trong `turbo.json` để trả lại khi tắt; giải phóng RAM bằng `EmptyWorkingSet`.
- [x] Quản lý khởi động: liệt kê cả mục đang tắt, bật/tắt qua `StartupApproved` như Task Manager.
- [x] Gỡ sạch: liệt kê ứng dụng Win32/MSI từ các nhánh Uninstall HKLM/HKCU, 32/64-bit; tìm theo tên/nhà phát hành, sắp theo tên/dung lượng/ngày cài.
- [x] Gỡ chuẩn bằng trình gỡ gốc (`ShellExecuteExW`, MSI dùng `msiexec /x`); sau gỡ quét thư mục và registry khớp tên, tránh thư mục hệ thống/dùng chung, không chọn sẵn AppData và thư mục từ 500 MB.
- [x] Gỡ sạch đợt 2: chọn nhiều ứng dụng, chạy hàng đợi tuần tự, báo kết quả/lỗi/mục chưa chạy; dừng hàng đợi khi hủy, gặp lỗi hoặc ứng dụng vẫn còn cài. Quét tàn dư riêng từng ứng dụng, không tự xóa.
- [x] Liệt kê và gỡ Store/Appx cùng các ứng dụng Windows cài sẵn có thể gỡ, chỉ cho tài khoản hiện tại. Lọc Desktop/Store/Có sẵn; gói thiết yếu, framework, resource, bundle và metadata không rõ được bảo vệ. Không gỡ provisioning hoặc gói của người dùng khác; dữ liệu `LocalAppData\\Packages` không chọn sẵn.
- [x] Chế độ cưỡng bức tách riêng, phải xác nhận cảnh báo; có mục tiêu ngoài danh sách bằng tên/thư mục đã xác minh. Chỉ cho desktop, không tự chuyển chế độ khi gỡ chuẩn lỗi, không chọn sẵn bất kỳ tàn dư nào; chặn thư mục hệ thống/dùng chung và liên kết thư mục.
- [x] Trước khi gỡ hoặc quét cưỡng bức phải thử tạo điểm khôi phục. Lỗi quyền quản trị/System Protection/tần suất được hiển thị; chỉ sau thất bại và xác nhận riêng mới cho tiếp tục. Phiên backend ràng buộc đúng mã ứng dụng/chế độ, gỡ chuẩn chỉ chạy một lần mỗi phiên.
- [x] Quét thêm lối tắt, giá trị Run/StartupApproved, service và tác vụ theo lịch dựa trên đường dẫn executable thuộc ứng dụng, không dò theo tên service/task hoặc tham số lệnh. Sao lưu rồi kiểm tra lại mục tiêu trước khi xóa; các mục này không chọn sẵn.
- [x] Service chỉ xóa khi stopped, Win32 own-process, tài khoản chuẩn, không có service phụ thuộc hoặc cấu hình nâng cao không phục hồi được; không tự dừng service. Task chỉ nhận các action thuộc ứng dụng và principal không cần mật khẩu; cả hai loại trừ auditing tùy chỉnh hoặc không xác minh được. Backup và bản ghi Rescue được ghi/flush trước khi xóa; lỗi sau bước xóa vẫn giữ bước phục hồi trong lịch sử. XML lưu đúng UTF-8; phục hồi task tạo mới với ACL gốc ngay lúc đăng ký, không ghi đè hoặc kích hoạt registration trigger.
- [x] Xem trước rồi xóa: thư mục vào Thùng rác, registry xuất ra `.reg` trước khi xóa; Rescue Center có tên bản ghi công cụ và hoàn tác bằng `UndoOp::ImportRegFile`.
- [x] Chốt an toàn: giao diện/backend chặn cả quét và xóa tàn dư khi khóa Uninstall vẫn còn; ví dụ chỉ-đọc không chọn sẵn bất kỳ tàn dư nào của ứng dụng còn cài. Dọn sạch danh sách/ô chọn cũ khi đổi ứng dụng hoặc quét lại.
- [x] Bỏ chờ vô hạn: kiểm tra tiến trình mỗi 200 ms, dùng cờ hủy chung; nút Ngừng chờ hoặc rời trang sẽ nhả việc chờ, không tắt trình gỡ. Ẩn nút quay lại danh sách khi đang xử lý và không tự quét sau khi rời trang.
- [x] Tìm tệp lớn: chọn thư mục người dùng/ổ cố định, ngưỡng 100/500/1024 MB, bỏ qua thư mục hệ thống, đánh dấu AppData, không chọn sẵn; mở Explorer và chuyển tệp vào Thùng rác.
- [x] Dọn rác thêm bộ nhớ đệm tải Windows Update (`SoftwareDistribution\\Download`), không chọn sẵn.
- [x] Giao diện đợt mới: Dọn dẹp có Tệp lớn/Tệp trùng/WinSxS; Tăng tốc có nhóm tinh chỉnh HKCU; Hộp công cụ có tinh chỉnh trình duyệt và Install Monitor. Không chọn sẵn tệp/tinh chỉnh/cache; WinSxS có xác nhận, điểm khôi phục và trạng thái tác vụ nền; Monitor có phiên trước/sau, báo cáo giới hạn và không tự xóa. Ô chọn tệp lớn đã có nhãn truy cập; backend giả và Việt/Anh đã tích hợp.
- [x] Tệp trùng: lọc theo kích thước/dấu vân tay rồi so nội dung chính xác; giữ bản không chọn đã khóa, loại hardlink/reparse/luồng NTFS phụ. Trước recycle chuyển đối tượng đã xác minh bằng handle vào vùng riêng cùng volume, tạo mới với quyền Administrators/SYSTEM; không fallback theo đường dẫn người dùng hoặc xóa vĩnh viễn. Cần quyền quản trị và filesystem hỗ trợ ACL; khóa staging dùng quyền đọc/liệt kê, chia sẻ đọc/ghi nhưng không share delete, mở quyền cleanup riêng để không tự chặn rename. Ghi bền vị trí gốc, volume/file ID trước chuyển và vị trí Thùng rác sau xác nhận. Kiểm tra lại stream sau shell move; dữ liệu phụ phát sinh vẫn được giữ và không báo success đã xác minh. Nếu không khôi phục được sau lỗi, giữ đường dẫn lưu giữ trong công cụ/Rescue, không tạo hoàn tác giả. Thùng rác của Windows ghi vị trí staging, không tự khôi phục về vị trí gốc; giao diện cảnh báo và giữ ánh xạ gốc/Bin để kiểm tra phục hồi.
- [x] WinSxS: phân tích và dọn bằng DISM cố định `/English`, `/StartComponentCleanup /NoRestart`; phân tích phải mới và có khuyến nghị, xác nhận/điểm khôi phục trước khi chạy. Lỗi khôi phục chỉ cho bỏ qua sau thất bại và xác nhận riêng. Giữ busy đến khi servicing thật kết thúc, ghi lịch sử trước khi chạy, không `/ResetBase`, không kill/timeout cleanup, báo khởi động lại với 3010.
- [x] Tối ưu sâu ban đầu và trình duyệt: 7 thiết lập HKCU cố định, sao lưu/kiểm tra lại trước ghi, chặn chính sách trình duyệt HKLM. Chỉ quét Cache/Code Cache/GPUCache/cache2 của Chrome/Edge/Brave/Firefox; không đụng cookie, mật khẩu, lịch sử hay tiện ích, không kill trình duyệt. DNS dùng ipconfig cố định, mở thiết lập mạng Windows; không tự sửa mạng/chính sách bảo mật.
- [x] Install Monitor: phạm vi thư mục riêng chính xác và registry cố định, trước/sau có giới hạn và lịch sử. Native liệt kê thư mục theo handle, đối chiếu identity/đường dẫn thật, không theo junction khi thư mục đổi; bỏ qua chênh lệch không chắc chắn. ID mili giây tăng dần trong miền số nguyên an toàn JavaScript; tên tối đa 128 byte UTF-8, vendor riêng tối đa 64 ký tự ASCII; chặn gốc dùng chung như `C:\\Tools`. Không tự chạy trình cài, suy đoán sở hữu hay xóa tàn dư.
- [x] Rescue journal: chỉ coi NotFound là lịch sử chưa tồn tại, báo lỗi dữ liệu hỏng/không đọc được; tạo scratch riêng bằng create_new, chặn hardlink/reparse, sync trước khi xuất bản bằng rename write-through trên Windows. Gia cố đường dẫn không giải quyết tính xác thực payload phục hồi còn tồn đọng bên dưới.

Đã kiểm tra:

- Đợt mới 18/11/2026: `npm run check` 0 lỗi/0 cảnh báo, `npm run build` đạt, 457 khóa Việt/Anh khớp. Đã tương tác DOM với backend giả cho năm công cụ: giữ một bản tệp trùng, xóa một phần/hủy; WinSxS xác nhận và khôi phục thất bại trước khi cho bỏ qua, busy qua đổi trang, lỗi và mã 3010; tinh chỉnh rồi hoàn tác; cache bị chặn khi trình duyệt chạy, DNS cần xác nhận; Monitor giữ baseline khi hủy/rời trang, thử lại, báo cáo chưa đầy đủ. Kiểm tra hồi quy Tệp lớn/Turbo/RAM/Gỡ sạch/Chăm sóc và xử lý lỗi đọc lịch sử không báo rỗng giả, không có lỗi console ứng dụng. Timer fixture được tăng tốc vì trình duyệt nhúng đang ẩn; không phải phép đo hiệu năng.
- Đợt mới 18/11/2026: `cargo test -p tp-core -- --include-ignored` đạt 184 test, 0 lỗi/0 ignored; 6 test Tauri đạt. `cargo check --workspace --all-targets`, `cargo build -p tp-system-care`, rustfmt cho module mới và `git diff --check` đều đạt, không cảnh báo Rust. Đã sửa fixture TEMP tên 8.3 bằng chuẩn hóa đường dẫn fixture, không nới luật đường dẫn thật; thêm test luồng NTFS phụ, nguồn được tạo lại, flush/khôi phục thất bại và native Monitor junction/retarget/rename/giới hạn lô. Có 5 trường hợp fixture staging cần elevation chưa thực thi trong phiên không quản trị; số test đạt không chứng minh các nhánh quyền quản trị/DACL đã chạy. Fixture không quản trị đã dùng đúng quyền/share của staging để xác nhận handle transfer không tự bị khóa, giữ ánh xạ gốc/định danh/vị trí sau chuyển và phát hiện ADS phát sinh muộn mà vẫn giữ dữ liệu.
- Mock bổ sung: Monitor dùng ID cỡ mili giây qua finish/detail không mất chính xác, hiển thị ngày năm 2026; từ chối gốc Tools, tên vượt 128 byte UTF-8 và vendor ngoài 64 ASCII/tên riêng. Lỗi recycle còn dữ liệu lưu giữ được hiển thị đầy đủ ở công cụ và Rescue, không có nút hoàn tác giả. Recycle thành công giữ ánh xạ gốc/Bin và cảnh báo giới hạn Khôi phục của Windows, không báo lỗi giả cho bản ghi thành công. Không lỗi console ứng dụng.
- 77 test của `tp-core` đạt (`cargo test -p tp-core -- --include-ignored`), không bỏ qua test nào; 2 test Tauri đạt. Gồm ràng buộc phiên/không chạy lại hàng đợi, hủy/lỗi/ứng dụng còn cài, đường dẫn cưỡng bức dùng chung/hệ thống, StartupApproved/Run sửa rồi hoàn tác, cập nhật bản phục hồi không trùng ID, chính sách gói Store và service/task. Fixture PowerShell/COM/SCM chạy trong bộ nhớ, không gỡ gói thật hoặc thay đổi service/task thật.
- `cargo build -p tp-system-care` đạt, không cảnh báo; `npm run check` 0 lỗi/0 cảnh báo; Việt/Anh có đủ 307 khóa trùng nhau. `npm run build` đạt, còn cảnh báo import động của Tauri không tách thành chunk riêng.
- Chạy công cụ chỉ-đọc trên máy phát triển: 119 ứng dụng, 19 mục khởi động (15 đang tắt); luật tránh thư mục dùng chung Adobe và không chọn sẵn thư mục lớn đã được đối chiếu. Quét tệp từ 500 MB trong hồ sơ người dùng mất 82,5 giây ở bản debug.
- Giao diện với backend giả: Tăng tốc, Tìm tệp lớn; Gỡ sạch đợt 2 với bộ lọc Desktop/Store/Có sẵn, bảo vệ gói thiết yếu, hàng đợi thành công/lỗi/còn cài/hủy, xét tàn dư riêng từng kết quả, cảnh báo cưỡng bức/mục tiêu ngoài danh sách, lỗi tạo điểm khôi phục phải xác nhận riêng, xóa/hoàn tác lịch sử và nhãn truy cập Việt/Anh. Đã thử rời trang khi đang chờ và chống giữ ô chọn/dữ liệu cũ; Store chỉ hiện dữ liệu gói tài khoản hiện tại, các mục service/task/startup/lối tắt và mọi mục cưỡng bức không chọn sẵn. Không lỗi console ứng dụng.

Còn tồn đọng:

- [ ] Tối ưu sâu ngoài nhóm HKCU ban đầu, lần dùng gần nhất và bộ lọc nâng cao ngoài Desktop/Store/Có sẵn.
- [ ] Service đang chạy, driver, service dùng chung/phụ thuộc/cấu hình nâng cao; task cần mật khẩu, action dùng runner chung; service/task có auditing tùy chỉnh hoặc không xác minh được vẫn bị loại khỏi quét tàn dư để bảo vệ hệ thống.
- [ ] Store chưa gỡ gói cấp sẵn hoặc gói của tài khoản khác; chưa đo tốc độ liệt kê/quét tàn dư mở rộng trên bản release.
- [ ] Hoàn tác task có thể làm task chạy theo lịch gốc; không kích hoạt registration trigger khi tái tạo. Xóa tàn dư không có nút hủy giữa một thao tác service/task đang chạy; từng lệnh giới hạn thời gian, kết quả không chắc chắn vẫn giữ bản phục hồi.
- [ ] Tiện ích/toolbar trình duyệt, bản cập nhật Windows và phần mềm đi kèm.
- [ ] Dọn theo ứng dụng, tiện ích nặng và chính sách mạng chi tiết chưa triển khai; tối ưu sâu hiện chỉ nhóm HKCU ban đầu.
- [ ] Chưa thử thao tác thật qua cửa sổ Tauri: Turbo, giải phóng RAM, bật/tắt khởi động, tạo điểm khôi phục, gỡ desktop/Store/hàng loạt/cưỡng bức, Ngừng chờ trình gỡ thật, xóa/hoàn tác service/task/lối tắt/registry, WinSxS, tinh chỉnh HKCU/cache/DNS và Monitor với trình cài thật. Chưa xác minh staging/DACL bằng phiên quản trị hoặc tương thích recycle của shell thật; mới kiểm tra fixture/primitives trên tệp tạm riêng, liệt kê chỉ-đọc và giao diện giả. Cần máy ảo thử nghiệm trước khi dùng trên hệ thống thật.
- [ ] Chưa kiểm tra trực quan bố cục bằng ảnh chụp: trình duyệt nhúng đang ẩn nên công cụ screenshot không có bề mặt hiển thị; đã kiểm tra tương tác và cấu trúc DOM.
- [ ] Chưa đo tốc độ quét tệp lớn ở bản release; chưa build lại bộ cài cho giai đoạn 4.

### Giai đoạn 5 — Bảo vệ, Cập nhật phần mềm, Hộp công cụ

- [ ] Tích hợp Defender, Firewall; chống theo dõi; bảo vệ trang chủ/DNS; webcam/micro.
- [ ] Cập nhật phần mềm qua `winget`.
- [ ] Driver: quét thiết bị, phát hiện driver thiếu/lỗi, tìm bản mới qua Windows Update.
- [ ] Driver: sao lưu, điểm khôi phục, cài một hoặc tất cả, báo khởi động lại.
- [ ] Driver: rollback từ bản sao lưu, danh sách bỏ qua, dẫn link hãng cho card đồ họa.
- [ ] Các công cụ trong Hộp công cụ.
- [ ] Khóa thư mục: đặt mật khẩu chính (Argon2id), mã khôi phục, đổi mật khẩu, đặt lại bằng mã khôi phục.
- [ ] Khóa thư mục: Khóa nhanh bằng quyền NTFS, danh sách thư mục đã khóa, mở tạm và khóa lại, chặn thư mục hệ thống.
- [ ] Khóa thư mục: Khóa mã hóa AES-256-GCM có nhật ký chạy tiếp khi bị ngắt.
- [ ] Khóa thư mục: menu chuột phải trong Explorer, chống dò mật khẩu, cảnh báo khi gỡ ứng dụng.
- [ ] (Pro) Bảo vệ thời gian thực qua Defender: bật, theo dõi, cảnh báo.
- [ ] (Pro) Safe Folder và chặn chương trình lạ bằng Controlled Folder Access.
- [ ] (Pro) Bảo vệ email: chặn liên kết lừa đảo ở tầng DNS/hosts.
- [ ] (Pro) Anti-Tracking tự dọn khi đóng trình duyệt; Identity Masking.
- [ ] (Pro) Cập nhật tất cả phần mềm một lần bấm và theo lịch.

### Giai đoạn 6 — Hoàn thiện và phát hành

- [ ] `tp-service` (Windows Service), lịch tự động.
- [ ] Khóa thư mục: tự khóa lại khi đăng xuất hoặc hết thời gian mở tạm (chạy trong `tp-service`).
- [ ] Icon khay hệ thống và widget CPU/RAM/nhiệt độ.
- [ ] Tự cập nhật, ký số mã nguồn (code signing).
- [ ] (Pro) Auto Care: tự quét và sửa theo lịch khi máy rảnh.
- [ ] (Pro) Tự giải phóng RAM theo ngưỡng.
- [ ] (Pro) CPU Saver với danh sách loại trừ.
- [ ] Kiểm thử trên Windows 10 và 11.

## Rủi ro

- **Dọn registry và tối ưu sâu có thể làm hỏng Windows:** mọi thao tác phải sao lưu trước và hoàn tác được; luật ban đầu giữ ở mức thận trọng.
- **Driver sai có thể gây màn hình xanh hoặc mất thiết bị:** chỉ lấy driver WHQL từ Windows Update, luôn sao lưu và tạo điểm khôi phục, không tự cài khi chạy theo lịch.
- **Gỡ cưỡng bức và quét tàn dư có thể xóa nhầm tệp dùng chung:** luôn xem trước, mục không chắc chắn để người dùng tự chọn, tệp vào Thùng rác thay vì xóa hẳn.
- **Tệp trùng có thể là bản sao lưu có chủ ý:** không chọn sẵn; luôn giữ ít nhất một bản đã xác minh trước khi đưa bản khác vào Thùng rác. Dung lượng logic không đồng nghĩa số byte thực tế được giải phóng trên ổ nén/sparse.
- **WinSxS là kho thành phần hệ thống:** chỉ dọn bằng DISM, không xóa tệp trực tiếp hoặc dùng `/ResetBase`; dọn không hoàn tác bằng Rescue Center và không được cưỡng bức dừng tiến trình servicing.
- **Install Monitor không chứng minh nguồn gốc thay đổi:** phần mềm khác có thể ghi trong cùng khoảng chụp; nhật ký có phạm vi hữu hạn, không tự coi mọi chênh lệch là tàn dư an toàn để xóa. Tên tệp/khóa registry trong nhật ký cũng là dữ liệu riêng tư.
- **Tệp lịch sử của Rescue Center nằm trong thư mục người dùng:** bước hoàn tác đọc từ `history.json` rồi ghi vào registry, kể cả HKLM khi ứng dụng chạy quyền quản trị. Tàn dư registry còn được hoàn tác bằng `UndoOp::ImportRegFile` (`reg.exe import`), nên cả đường dẫn lẫn nội dung tệp `.reg` là dữ liệu nhạy cảm. Đợt 2 thêm payload service/task và XML trong cùng cây sao lưu; cần bảo vệ tính toàn vẹn của các dữ liệu này trước khi phát hành, nhất là khi phục hồi tài nguyên chạy bằng SYSTEM. Chương trình chạy quyền thường có thể sửa các tệp này để mượn quyền. Cần chuyển sang thư mục chỉ quản trị viên ghi được khi có `tp-service` (giai đoạn 6).
- **Khóa thư mục có thể làm mất dữ liệu:** với Khóa mã hóa, mất cả mật khẩu lẫn mã khôi phục là không lấy lại được; phải bắt người dùng lưu mã khôi phục và nói rõ điều này. Mã hóa bị ngắt giữa chừng phải chạy tiếp được, không để tệp ở trạng thái dở.
- **Khóa nhanh không phải mã hóa:** phải ghi rõ trên giao diện để người dùng không tưởng nó chống được người có quyền quản trị hoặc việc tháo ổ cứng.
- **Antivirus báo nhầm:** cần chứng chỉ ký mã cho bản phát hành.

## Lệnh thường dùng

Chạy bản dev (từ thư mục `app`):

```bash
npm run tauri dev
```

Build bản release và bộ cài (từ thư mục `app`):

```bash
npm run tauri build
```

Chạy test lõi và quét thử chỉ-đọc (từ thư mục gốc):

```bash
cargo test -p tp-core
```

```bash
cargo run -p tp-core --example scan
cargo run -p tp-core --example tools
cargo run -p tp-core --example leftover_audit
```

Hai ví dụ công cụ chỉ liệt kê và xem trước; khi ứng dụng vẫn còn cài, không tàn dư nào được chọn sẵn. Giao diện chặn cả quét lẫn xóa cho đến khi khóa Uninstall biến mất.

Sản phẩm build: `target\release\tp-system-care.exe` và `target\release\bundle\nsis\TP System Care_0.1.0_x64-setup.exe`.

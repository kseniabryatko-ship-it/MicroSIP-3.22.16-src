/*
 * Copyright (C) 2011-2026 MicroSIP (http://www.microsip.org)
 *
 * This program is free software; you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation; either version 2 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program; if not, write to the Free Software
 * Foundation, Inc., 59 Temple Place, Suite 330, Boston, MA  02111-1307  USA
 */

#include "stdafx.h"
#include "Contacts.h"
#include "microsip.h"
#include "settings.h"
#include <pjsua-lib/pjsua.h>
#include <pjsua-lib/pjsua_internal.h>
#include "mainDlg.h"
#include "langpack.h"
#include "CSVFile.h"
#include "Markup.h"
#include "Transfer.h"
#include "afxinet.h"
#include "MessageBoxX.h"
#include "Domain/Contact.h"
#include "Data/Database.h"
#include "Data/ContactsRepository.h"
#include "Utils/StringConverter.h"


static UINT_PTR blinkTimer = NULL;
static bool blinkState = false;

Contacts::Contacts(CWnd* pParent /*=NULL*/)
	: CBaseDialog(Contacts::IDD, pParent)
{
	Create(IDD, pParent);
}

Contacts::~Contacts(void)
{
}

BOOL Contacts::OnInitDialog()
{
	CBaseDialog::OnInitDialog();

	AutoMove(IDC_CONTACTS, 0, 0, 100, 100);
	AutoMove(IDC_SEARCH_PICTURE, 0, 100, 0, 0);
	AutoMove(IDC_FILER_VALUE, 0, 100, 100, 0);

	TranslateDialog(this->m_hWnd);

	addDlg = new AddDlg(this);

	CListCtrl* list = (CListCtrl*)GetDlgItem(IDC_CONTACTS);
	//list->SetExtendedStyle(list->GetExtendedStyle() | LVS_EX_FULLROWSELECT | LVS_EX_AUTOSIZECOLUMNS);
	list->SetExtendedStyle(list->GetExtendedStyle() | LVS_EX_FULLROWSELECT);
	list->SetImageList(&mainDlg->imageListStatus, LVSIL_SMALL);

	CFont* font = list->GetFont();
	LOGFONT lf;
	font->GetLogFont(&lf);
	lf.lfHeight = -MulDiv(12, dpiY, 96);
	font = new CFont();
	font->CreateFontIndirect(&lf);
	list->SetFont(font);
	((CEdit*)GetDlgItem(IDC_FILER_VALUE))->SetFont(font);

	list->InsertColumn(0, Translate(_T("Name")), LVCFMT_LEFT, accountSettings.contactsWidth0 > 0 ? accountSettings.contactsWidth0 : 160);
	list->InsertColumn(1, Translate(_T("Number")), LVCFMT_LEFT, accountSettings.contactsWidth1 > 0 ? accountSettings.contactsWidth1 : 100);
	list->InsertColumn(2, Translate(_T("Information")), LVCFMT_LEFT, accountSettings.contactsWidth2 > 0 ? accountSettings.contactsWidth2 : 120);

    CString filename = accountSettings.pathRoaming + _T("contacts.db");
    static Database db(CStringToUtf8(filename));
    db.InitContacts();
    static ContactsService contactsService(std::make_unique<ContactsRepository>(db.Get()));

    m_contactsService = &contactsService;

	ContactsLoad();

	return TRUE;
}

void Contacts::OnCreated()
{
	m_list.SetSortColumn(0, true);
}

void Contacts::PostNcDestroy()
{
	CBaseDialog::PostNcDestroy();
	//mainDlg->pageContacts = NULL;
}

void Contacts::DoDataExchange(CDataExchange* pDX)
{
	CBaseDialog::DoDataExchange(pDX);
	DDX_Control(pDX, IDC_CONTACTS, m_list);
}

BEGIN_MESSAGE_MAP(Contacts, CBaseDialog)
	ON_WM_TIMER()
	ON_NOTIFY(HDN_ENDTRACK, 0, OnEndtrack)
	ON_BN_CLICKED(IDOK, OnBnClickedOk)
	ON_BN_CLICKED(IDCANCEL, OnBnClickedCancel)
	ON_EN_CHANGE(IDC_FILER_VALUE, OnFilterValueChange)
	ON_COMMAND(ID_CALL_PICKUP, OnMenuCallPickup)
	ON_COMMAND(ID_CALL, OnMenuCall)
	ON_COMMAND(ID_CALL_PHONE, OnMenuCallPhone)
	ON_COMMAND(ID_CALL_MOBILE, OnMenuCallMobile)
	ON_COMMAND(ID_CHAT, OnMenuChat)
	ON_COMMAND(ID_ADD, OnMenuAdd)
	ON_COMMAND(ID_EDIT, OnMenuEdit)
	ON_COMMAND(ID_COPY, OnMenuCopy)
	ON_COMMAND(ID_DELETE, OnMenuDelete)
	ON_COMMAND(ID_EXPORT, OnMenuExport)	
	ON_COMMAND(ID_IMPORT, OnMenuImport)
	ON_MESSAGE(WM_CONTEXTMENU, OnContextMenu)
	ON_NOTIFY(NM_DBLCLK, IDC_CONTACTS, &Contacts::OnNMDblclkContacts)
#ifdef _GLOBAL_VIDEO
	ON_COMMAND(ID_VIDEOCALL, OnMenuCallVideo)
#endif
END_MESSAGE_MAP()

void Contacts::OnTimer(UINT_PTR TimerVal)
{
	if (TimerVal == IDT_TIMER_CONTACTS_BLINK) {
		OnTimerContactsBlink();
	}
}

BOOL Contacts::PreTranslateMessage(MSG* pMsg)
{
	BOOL catched = FALSE;
	if (pMsg->message == WM_KEYDOWN) {
		if (pMsg->wParam == VK_ESCAPE) {
			CEdit* edit = (CEdit*)GetDlgItem(IDC_FILER_VALUE);
			if (edit == GetFocus()) {
				catched = TRUE;
                CString search;
                edit->GetWindowText(search);
                if (!search.IsEmpty()) {
                    FilterReset();
                }
            }
		}
		if (pMsg->wParam == VK_DELETE) {
            if (&m_list == GetFocus()) {
				catched = TRUE;
				OnMenuDelete();
			}
		}
	}
	if (!catched) {
		return CBaseDialog::PreTranslateMessage(pMsg);
	}
	else {
		return TRUE;
	}
}

void Contacts::OnEndtrack(NMHDR* pNMHDR, LRESULT* pResult)
{
	HD_NOTIFY* phdn = (HD_NOTIFY*)pNMHDR;
	int width = phdn->pitem->cxy;
	switch (phdn->iItem) {
	case 0:
		accountSettings.contactsWidth0 = width;
		break;
	case 1:
		accountSettings.contactsWidth1 = width;
		break;
	case 2:
		accountSettings.contactsWidth2 = width;
		break;
	}
	mainDlg->AccountSettingsPendingSave();
	*pResult = 0;
}

void Contacts::OnBnClickedOk()
{
	CListCtrl* list = (CListCtrl*)GetDlgItem(IDC_CONTACTS);
	POSITION pos = list->GetFirstSelectedItemPosition();
	if (pos) {
		DefaultItemAction(list->GetNextSelectedItem(pos));
	}
}

void Contacts::DefaultItemAction(int i)
{
	CListCtrl* list = (CListCtrl*)GetDlgItem(IDC_CONTACTS);
	Contact* contact = (Contact*)list->GetItemData(i);
	bool pickup = contact->ringing;
	pickup = false;
	if (pickup && mainDlg->CommandCallPickup(contact->number)) {
	}
	else {
		MessagesContact* messagesContact = mainDlg->messagesDlg->GetMessageContact();
		if (messagesContact && messagesContact->callId != -1) {
			mainDlg->OpenTransferDlg(mainDlg, MSIP_ACTION_TRANSFER, PJSUA_INVALID_ID, contact);
		}
		else {
			if (accountSettings.defaultAction.IsEmpty()) {
				MessageDlgOpen(accountSettings.singleMode);
			}
			else {
				if (accountSettings.defaultAction == _T("call")) {
					OnMenuCall();
				}
#ifdef _GLOBAL_VIDEO
				else if (accountSettings.defaultAction == _T("video")) {
					OnMenuCallVideo();
				}
#endif
				else {
					OnMenuChat();
				}
			}
		}
	}
}

void Contacts::OnBnClickedCancel()
{
	mainDlg->ShowWindow(SW_HIDE);
}

bool Contacts::IsFiltered() {
    CEdit* edit = (CEdit*)GetDlgItem(IDC_FILER_VALUE);
    CString search;
    edit->GetWindowText(search);
    if (!search.IsEmpty()) {
        return true;
    }
    return false;
}

bool Contacts::IsFiltered(const  Contact& contact) {
    CEdit* edit = (CEdit*)GetDlgItem(IDC_FILER_VALUE);
    CString search;
    edit->GetWindowText(search);
    return m_contactsService->IsContactFiletered(contact, search);
}

void Contacts::FilterReset()
{
	CEdit* edit = (CEdit*)GetDlgItem(IDC_FILER_VALUE);
	edit->SetWindowText(_T(""));
}

void Contacts::OnFilterValueChange()
{
    ContactsClear();
	POSITION pos = contacts.GetHeadPosition();
	while (pos) {
		Contact* contact = contacts.GetNext(pos);
		if (!IsFiltered(*contact)) {
			ListAppend(&m_list, contact);
		}
	}
	m_list.SortColumn(m_list.GetSortColumn(), m_list.IsAscending());
}

LRESULT Contacts::OnContextMenu(WPARAM wParam, LPARAM lParam)
{
	int x = GET_X_LPARAM(lParam);
	int y = GET_Y_LPARAM(lParam);
	POINT pt = { x, y };
	RECT rc;
	CListCtrl* list = (CListCtrl*)GetDlgItem(IDC_CONTACTS);
	POSITION pos = list->GetFirstSelectedItemPosition();
	int selectedItem = -1;
	if (pos) {
		selectedItem = list->GetNextSelectedItem(pos);
	}
	if (x != -1 || y != -1) {
		ScreenToClient(&pt);
		GetClientRect(&rc);
		if (!PtInRect(&rc, pt)) {
			x = y = -1;
		}
	}
	else {
		if (selectedItem != -1) {
			list->GetItemPosition(selectedItem, &pt);
			list->ClientToScreen(&pt);
			x = 40 + pt.x;
			y = 8 + pt.y;
		}
		else {
			::ClientToScreen((HWND)wParam, &pt);
			x = 10 + pt.x;
			y = 10 + pt.y;
		}
	}
	if (x != -1 || y != -1) {
		CMenu menu;
		menu.LoadMenu(IDR_MENU_CONTACT);
		CMenu* tracker = menu.GetSubMenu(0);
		TranslateMenu(tracker->m_hMenu);
		if (selectedItem != -1) {
			Contact* pContact = (Contact*)list->GetItemData(selectedItem);
			if (pContact->ringing) {
				if (accountSettings.enableFeatureCodeCP && !accountSettings.featureCodeCP.IsEmpty()) {
					tracker->InsertMenu(ID_CALL, 0, ID_CALL_PICKUP, Translate(_T("Call Pickup")));
					tracker->InsertMenu(ID_CALL, MF_SEPARATOR);
				}
			}
			//--
			CMenu numbersMenu;
			numbersMenu.CreatePopupMenu();
			numbersMenu.AppendMenu(MF_STRING, ID_CALL, pContact->number);
			if (!pContact->phone.IsEmpty() && pContact->phone != pContact->number) {
				CString str;
				str.Format(_T("%s %s"),  Translate(_T("Phone:")), pContact->phone );
				numbersMenu.AppendMenu(MF_STRING, ID_CALL_PHONE, str);
			}
			if (!pContact->mobile.IsEmpty() && pContact->mobile != pContact->number) {
				CString str;
				str.Format(_T("%s %s"),  Translate(_T("Mobile:")), pContact->mobile );
				numbersMenu.AppendMenu(MF_STRING, ID_CALL_MOBILE, str);
			}
			if (numbersMenu.GetMenuItemCount() > 1) {
				tracker->ModifyMenu(ID_CALL, MF_BYCOMMAND | MF_POPUP, (UINT_PTR)numbersMenu.m_hMenu, Translate(_T("Call")));
			}
			//--
			tracker->EnableMenuItem(ID_CALL, FALSE);
#ifdef _GLOBAL_VIDEO
			tracker->EnableMenuItem(ID_VIDEOCALL, FALSE);
#endif
			tracker->EnableMenuItem(ID_CHAT, FALSE);
			tracker->EnableMenuItem(ID_COPY, FALSE);
			tracker->EnableMenuItem(ID_EDIT, FALSE);
			tracker->EnableMenuItem(ID_DELETE, FALSE);
		}
		else {
			tracker->EnableMenuItem(ID_CALL, TRUE);
#ifdef _GLOBAL_VIDEO
			tracker->EnableMenuItem(ID_VIDEOCALL, TRUE);
#endif
			tracker->EnableMenuItem(ID_CHAT, TRUE);
			tracker->EnableMenuItem(ID_COPY, TRUE);
			tracker->EnableMenuItem(ID_EDIT, TRUE);
			tracker->EnableMenuItem(ID_DELETE, TRUE);
		}
		tracker->AppendMenu(0, MF_SEPARATOR);
		tracker->AppendMenu(MF_STRING, ID_EXPORT, Translate(_T("Export")));
		tracker->AppendMenu(MF_STRING, ID_IMPORT, Translate(_T("Import")));		
#ifdef _GLOBAL_VIDEO
		if (accountSettings.disableVideo) {
			tracker->RemoveMenu(ID_VIDEOCALL, MF_BYCOMMAND);
		}
#endif
		if (accountSettings.disableMessaging) {
			tracker->RemoveMenu(ID_CHAT, MF_BYCOMMAND);
		}
		if (tracker->GetMenuItemCount() == 3) {
			tracker->RemoveMenu(0, MF_BYPOSITION);
		}

		tracker->TrackPopupMenu(0, x, y, this);
		return TRUE;
	}
	return DefWindowProc(WM_CONTEXTMENU, wParam, lParam);
}

void Contacts::MessageDlgOpen(BOOL isCall, BOOL hasVideo, BYTE index)
{
	if (accountSettings.singleMode && mainDlg->messagesDlg->GetCallsCount() && isCall) {
		mainDlg->GotoTab(0);
		return;
	}
	CListCtrl* list = (CListCtrl*)GetDlgItem(IDC_CONTACTS);
	POSITION pos = list->GetFirstSelectedItemPosition();
	if (pos) {
		int i = list->GetNextSelectedItem(pos);
		Contact* pContact = (Contact*)list->GetItemData(i);
		CString number = pContact->number;
		if (index == 1 && !pContact->phone.IsEmpty()) {
			number = pContact->phone;
		}
		if (index == 2 && !pContact->mobile.IsEmpty()) {
			number = pContact->mobile;
		}
		if (isCall) {
			mainDlg->MakeCall(number, hasVideo, false, false, pContact->name);
		}
		else {
			mainDlg->MessagesOpen(number, false, false, pContact->name);
		}
	}
}

void Contacts::OnNMDblclkContacts(NMHDR* pNMHDR, LRESULT* pResult)
{
	LPNMITEMACTIVATE pNMItemActivate = reinterpret_cast<LPNMITEMACTIVATE>(pNMHDR);
	if (pNMItemActivate->iItem != -1) {
		DefaultItemAction(pNMItemActivate->iItem);
	}
	*pResult = 0;
}

void Contacts::OnMenuCallPickup()
{
	CListCtrl* list = (CListCtrl*)GetDlgItem(IDC_CONTACTS);
	POSITION pos = list->GetFirstSelectedItemPosition();
	if (pos) {
		int i = list->GetNextSelectedItem(pos);
		Contact* pContact = (Contact*)list->GetItemData(i);
		if (pContact->ringing) {
			mainDlg->CommandCallPickup(pContact->number);
		}
	}
}

void Contacts::OnMenuCall()
{
	MessageDlgOpen(TRUE);
}

void Contacts::OnMenuCallPhone()
{
	MessageDlgOpen(TRUE, 0, 1);
}

void Contacts::OnMenuCallMobile()
{
	MessageDlgOpen(TRUE, 0, 2);
}

#ifdef _GLOBAL_VIDEO
void Contacts::OnMenuCallVideo()
{
	MessageDlgOpen(TRUE, TRUE);
}
#endif

void Contacts::OnMenuChat()
{
	if (!accountSettings.disableMessaging) {
		MessageDlgOpen();
	}
}

void Contacts::OnMenuAdd()
{
	if (!addDlg->IsWindowVisible()) {
		addDlg->ShowWindow(SW_SHOW);
	}
	else {
		addDlg->SetForegroundWindow();
	}
	Contact contact;
	addDlg->Load(&contact);
}

void Contacts::OnMenuEdit()
{
	OnMenuAdd();
	CListCtrl* list = (CListCtrl*)GetDlgItem(IDC_CONTACTS);
	POSITION pos = list->GetFirstSelectedItemPosition();
	int i = list->GetNextSelectedItem(pos);
	Contact* pContact = (Contact*)list->GetItemData(i);
	addDlg->Load(pContact);
}

void Contacts::OnMenuCopy()
{
	CListCtrl* list = (CListCtrl*)GetDlgItem(IDC_CONTACTS);
	POSITION pos = list->GetFirstSelectedItemPosition();
	if (pos) {
		int i = list->GetNextSelectedItem(pos);
		Contact* pContact = (Contact*)list->GetItemData(i);
		mainDlg->CopyStringToClipboard(pContact->number);
	}
}

void Contacts::OnMenuDelete()
{
	CList<CString, CString> contactsSelected;
	CListCtrl* list = (CListCtrl*)GetDlgItem(IDC_CONTACTS);
	POSITION pos = list->GetFirstSelectedItemPosition();
	if (pos) {
		if (MessageBox(Translate(_T("Are you sure you want to delete the selected items?")), Translate(_T("Delete Contact")), MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2) != IDYES) {
			return;
		}
		while (pos) {
			Contact* pContact = (Contact*)list->GetItemData(list->GetNextSelectedItem(pos));
			contactsSelected.AddTail(pContact->number);
		}
		if (IsFiltered()) {
			FilterReset();
		}
		int count = list->GetItemCount();
		bool deleted = false;
		for (int i = 0; i < count; i++) {
			Contact* pContact = (Contact*)list->GetItemData(i);
			if (contactsSelected.Find(pContact->number)) {
				bool allow = true;
				if (allow) {
					ContactDelete(i);
					count--;
					i--;
					deleted = true;
				}
			}
		}
		if (deleted) {
			ContactsSave();
		}
	}
}

bool Contacts::Import(CString filename, CArray<ContactWithFields*>& contactsWithFields, bool directory)
{
    auto error = m_contactsService->ImportContactsCSV(filename, contactsWithFields, directory);
    if (error.IsEmpty()) {
        return true;
    }
    else {
        return false;
    }
}

void Contacts::OnMenuExport()
{
    Export(true);
}

void Contacts::Export(bool bFilter)
{
    CString defaultFilename;
    defaultFilename.Format(_T("%s Contacts"), _T(_GLOBAL_NAME_NICE));
    CString search;
    if (bFilter) {
        CEdit* edit = (CEdit*)GetDlgItem(IDC_FILER_VALUE);
        edit->GetWindowText(search);
    }
    else {
        if (IsFiltered()) {
            FilterReset();
        }
    }
    if (!search.IsEmpty()) {
        defaultFilename.AppendFormat(_T(" %s"), search);
    }
	TCHAR szFilters[] = _T("CSV Files (*.csv)|*.csv|XML Files (*.xml)|*.xml||");
	CFileDialog dlgFile(FALSE, _T("csv"), MSIP::MakeFilenameFromString(defaultFilename), OFN_PATHMUSTEXIST | OFN_OVERWRITEPROMPT | OFN_HIDEREADONLY, szFilters, this);
	if (dlgFile.DoModal() == IDOK) {
		CString filename = dlgFile.GetPathName();
		if (dlgFile.m_ofn.nFilterIndex == 2) {
			if (dlgFile.GetFileExt().IsEmpty()) {
				filename.Append(_T(".xml"));
			}
            m_contactsService->ExportContactsXML((filename), (search), &m_list);
		}
		else {
			if (dlgFile.GetFileExt().IsEmpty()) {
				filename.Append(_T(".csv"));
			}
            m_contactsService->ExportContactsCSV((filename), (search), &m_list);
		}
	}
}

ContactsService* Contacts::GetContactsService()
{
    return m_contactsService;
}

void Contacts::OnMenuImport()
{
	CFileDialog dlgFile(TRUE, _T("cvs"), 0, OFN_FILEMUSTEXIST | OFN_HIDEREADONLY, _T("CSV Files (*.csv)|*.csv|"), this);
	if (dlgFile.DoModal() == IDOK) {
		if (IsFiltered()) {
			FilterReset();
		}
        CArray<ContactWithFields*> contactsWithFields;
        CString error = m_contactsService->ImportContactsCSV((dlgFile.GetPathName()), contactsWithFields);
        if (error.IsEmpty()) {
            if (contactsWithFields.GetCount()) {
                ContactsAdd(&contactsWithFields);
                ContactWithFields* contactWithFields;
                for (int i = 0; i < contactsWithFields.GetCount(); i++) {
                    contactWithFields = contactsWithFields.GetAt(i);
                    delete contactWithFields;
                }
                m_list.SortColumn(m_list.GetSortColumn(), m_list.IsAscending());
            }
        }
        else {
            AfxMessageBox(error);
        }
	}
}

void Contacts::ContactCreate(CListCtrl* list, Contact* pContact, bool subscribe)
{
	Contact* contact = new Contact();
	contacts.AddTail(contact);
	contact->image = MSIP_CONTACT_ICON_DEFAULT;
	contact->name = pContact->name;
	contact->number = pContact->number;
	contact->firstname = pContact->firstname;
	contact->lastname = pContact->lastname;
	contact->phone = pContact->phone;
	contact->mobile = pContact->mobile;
	contact->email = pContact->email;
	contact->address = pContact->address;
	contact->city = pContact->city;
	contact->state = pContact->state;
	contact->zip = pContact->zip;
	contact->comment = pContact->comment;
	contact->id = pContact->id;
	if (!contact->presence || contact->info.IsEmpty()) {
		contact->info = pContact->info;
	}
	contact->presence = pContact->presence;
	contact->directory = pContact->directory;
	contact->starred = pContact->starred;
	ListAppend(list, contact, subscribe);
}

void Contacts::ListAppend(CListCtrl* list, Contact* contact, bool subscribe)
{
	int i = list->InsertItem(LVIF_TEXT | LVIF_PARAM | LVIF_IMAGE, 0, contact->name, 0, 0, contact->image + (contact->starred ? 7 : 0), (LPARAM)contact);
	CString number = contact->number;
	list->SetItemText(i, 1, number);
	list->SetItemText(i, 2, Translate(contact->info));
	if (subscribe) {
		if (contact->presence) {
			mainDlg->SubsribeNumber(contact->number);
		}
	}
}


bool Contacts::ContactUpdate(CListCtrl* list, int i, Contact* contact, Contact* newContact, CStringList* fields)
{
	bool changed = false;
	if (!fields || fields->Find(_T("name"))) {
		if (contact->name != newContact->name) {
			list->SetItemText(i, 0, newContact->name);
			contact->name = newContact->name;
			changed = true;
		}
	}
	if (!fields || fields->Find(_T("number"))) {
		if (contact->number != newContact->number) {
			bool presenceOrig = contact->presence;
			if (contact->presence) {
				contact->presence = false;
				PresenceUnsubsribeOne(contact);
			}
			list->SetItemText(i, 1, newContact->number);
			contact->number = newContact->number;
			if ((!fields || fields->Find(_T("presence")))) {
				contact->presence = newContact->presence;
			}
			else {
				contact->presence = presenceOrig;
			}
			if (contact->presence) {
				mainDlg->SubsribeNumber(contact->number);
			}
			changed = true;
		}
	}
	if (!fields || fields->Find(_T("firstname"))) {
		if (contact->firstname != newContact->firstname) {
			contact->firstname = newContact->firstname;
			changed = true;
		}
	}
	if (!fields || fields->Find(_T("lastname"))) {
		if (contact->lastname != newContact->lastname) {
			contact->lastname = newContact->lastname;
			changed = true;
		}
	}
	if (!fields || fields->Find(_T("phone"))) {
		if (contact->phone != newContact->phone) {
			contact->phone = newContact->phone;
			changed = true;
		}
	}
	if (!fields || fields->Find(_T("mobile"))) {
		if (contact->mobile != newContact->mobile) {
			contact->mobile = newContact->mobile;
			changed = true;
		}
	}
	if (!fields || fields->Find(_T("email"))) {
		if (contact->email != newContact->email) {
			contact->email = newContact->email;
			changed = true;
		}
	}
	if (!fields || fields->Find(_T("address"))) {
		if (contact->address != newContact->address) {
			contact->address = newContact->address;
			changed = true;
		}
	}
	if (!fields || fields->Find(_T("city"))) {
		if (contact->city != newContact->city) {
			contact->city = newContact->city;
			changed = true;
		}
	}
	if (!fields || fields->Find(_T("state"))) {
		if (contact->state != newContact->state) {
			contact->state = newContact->state;
			changed = true;
		}
	}
	if (!fields || fields->Find(_T("zip"))) {
		if (contact->zip != newContact->zip) {
			contact->zip = newContact->zip;
			changed = true;
		}
	}
	if (!fields || fields->Find(_T("comment"))) {
		if (contact->comment != newContact->comment) {
			contact->comment = newContact->comment;
			changed = true;
		}
	}
	if (!fields || fields->Find(_T("id"))) {
		if (contact->id != newContact->id) {
			contact->id = newContact->id;
			changed = true;
		}
	}

	if (!fields || fields->Find(_T("info"))) {
		if ((!contact->presence || contact->info.IsEmpty()) && contact->info != newContact->info) {
			list->SetItemText(i, 2, Translate(newContact->info));
			contact->info = newContact->info;
			changed = true;
		}
	}
	if (!fields || fields->Find(_T("starred"))) {
		if (newContact->starred != contact->starred) {
			contact->starred = newContact->starred;
			list->SetItem(i, 0, LVIF_IMAGE, 0, contact->image + (contact->starred ? 7 : 0), 0, 0, 0);
			changed = true;
		}
	}
	if (!fields || fields->Find(_T("presence"))) {
		if (newContact->presence != contact->presence) {
			contact->presence = newContact->presence;
			if (contact->presence) {
				mainDlg->SubsribeNumber(contact->number);
			}
			else {
				PresenceUnsubsribeOne(contact);
			}
			changed = true;
		}
	}
	return changed;
}

void Contacts::ContactsAdd(CArray<ContactWithFields*>* contactsWithFields, bool directory)
{
	if (IsFiltered()) {
		FilterReset();
	}
	CListCtrl* list = (CListCtrl*)GetDlgItem(IDC_CONTACTS);
	bool changedAny = false;
	int count = list->GetItemCount();
	int countNew = contactsWithFields->GetCount();
	for (int i = 0; i < count; i++) {
		Contact* contact = (Contact*)list->GetItemData(i);
		bool found = false;
		for (int j = 0; j < countNew; j++) {
			ContactWithFields* contactWithFields = contactsWithFields->GetAt(j);
			if (contact->number == contactWithFields->contact.number 
				&& contact->name == contactWithFields->contact.name
				) {
				contactWithFields->processed = true;
				found = true;
				if (ContactUpdate(list, i, contact, &contactWithFields->contact, &contactWithFields->fields)) {
					changedAny = true;
				}
			}
		}
		if (directory && contact->directory && !found) {
			ContactDelete(i);
			changedAny = true;
			count--;
			i--;
		}
	}
	for (int j = 0; j < countNew; j++) {
		ContactWithFields* contactWithFields = contactsWithFields->GetAt(j);
		if (!contactWithFields->processed) {
			ContactCreate(list, &contactWithFields->contact);
			changedAny = true;
		}
	}
	if (changedAny) {
		ContactsSave();
	}
}

bool Contacts::ContactAdd(Contact contact, BOOL save, BOOL load, CStringList* fields, CString oldNumber, bool manual)
{
	if (!m_contactsService->ContactPrepare(contact)) {
		return false;
	}
	if (save) {
		if (IsFiltered()) {
			FilterReset();
		}
	}
	CListCtrl* list = (CListCtrl*)GetDlgItem(IDC_CONTACTS);
	if (!load) {
		bool found = false;
		bool changedAny = false;
		int count = list->GetItemCount();
		for (int i = 0; i < count; i++) {
			Contact* pContact = (Contact*)list->GetItemData(i);
			CString compareNumber = !oldNumber.IsEmpty() ? oldNumber : contact.number;
			if (pContact->number == compareNumber) {
				found = true;
				pContact->candidate = false;
				bool changed = ContactUpdate(list, i, pContact, &contact, fields);
				if (changed) {
					changedAny = true;
				}
			}
		}
		if (found) {
			if (save && changedAny) {
				ContactsSave();
			}
			if (manual && changedAny) {
				m_list.SortColumn(m_list.GetSortColumn(), m_list.IsAscending());
			}
			return true;
		}
	}
	ContactCreate(list, &contact, !load);
	if (save) {
		ContactsSave();
	}
	if (manual) {
		m_list.SortColumn(m_list.GetSortColumn(), m_list.IsAscending());
	}
	return true;
}

void Contacts::ContactDelete(int i)
{
	CListCtrl* list = (CListCtrl*)GetDlgItem(IDC_CONTACTS);
	Contact* contact = (Contact*)list->GetItemData(i);
	list->DeleteItem(i);
	ContactDeleteRaw(contact);
}

void Contacts::ContactDeleteRaw(Contact* contact)
{
	if (contact->presence) {
		contact->presence = false;
		PresenceUnsubsribeOne(contact);
	}
	POSITION pos = contacts.Find(contact);
	contacts.RemoveAt(pos);
	delete contact;
}

void Contacts::ContactsSave()
{
	if (IsFiltered()) {
		FilterReset();
	}
	CMarkup xml;
	xml.AddElem(_T("contacts"));
	xml.IntoElem();

	CListCtrl* list = (CListCtrl*)GetDlgItem(IDC_CONTACTS);
	int count = list->GetItemCount();
	for (int i = 0; i < count; i++) {
		Contact* pContact = (Contact*)list->GetItemData(i);
		xml.AddElem(_T("contact"));
		xml.AddAttrib(_T("name"), pContact->name);
		xml.AddAttrib(_T("number"), pContact->number);
		xml.AddAttrib(_T("firstname"), pContact->firstname);
		xml.AddAttrib(_T("lastname"), pContact->lastname);
		xml.AddAttrib(_T("phone"), pContact->phone);
		xml.AddAttrib(_T("mobile"), pContact->mobile);
		xml.AddAttrib(_T("email"), pContact->email);
		xml.AddAttrib(_T("address"), pContact->address);
		xml.AddAttrib(_T("city"), pContact->city);
		xml.AddAttrib(_T("state"), pContact->state);
		xml.AddAttrib(_T("zip"), pContact->zip);
		xml.AddAttrib(_T("comment"), pContact->comment);
		xml.AddAttrib(_T("id"), pContact->id);
		xml.AddAttrib(_T("info"), pContact->info);
		xml.AddAttrib(_T("presence"), pContact->presence ? _T("1") : _T("0"));
		xml.AddAttrib(_T("starred"), pContact->starred ? _T("1") : _T("0"));
		xml.AddAttrib(_T("directory"), pContact->directory ? _T("1") : _T("0"));
	}

	CString filename = accountSettings.pathRoaming;
	filename.Append(_T("Contacts.xml"));
	CFile file;
	CFileException fileException;
	if (file.Open(filename, CFile::modeCreate | CFile::modeWrite, &fileException)) {
		CStringA str = "<?xml version=\"1.0\"?>\r\n";
		str.Append(MSIP::Utf8EncodeUni(xml.GetDoc()));
		file.Write(str.GetString(), str.GetLength());
		file.Close();
	}
}

void Contacts::ContactsClear()
{
    m_list.DeleteAllItems();
}


void Contacts::ContactsLoad()
{
	CString filename = accountSettings.pathRoaming;
	filename.Append(_T("Contacts.xml"));
	CFile file;
	CFileException fileException;
	if (file.Open(filename, CFile::modeRead, &fileException)) {
		CStringA data;
		int i;
		UINT len = 0;
		do {
			LPSTR p = data.GetBuffer(len + 1024);
			i = file.Read(p + len, 1024);
			len += i;
			data.ReleaseBuffer(len);
		} while (i > 0);
		file.Close();
		CMarkup xml;
		BOOL bResult = xml.SetDoc(MSIP::Utf8DecodeUni(data));
		if (bResult) {
			if (xml.FindElem(_T("contacts"))) {
				while (xml.FindChildElem(_T("contact"))) {
					xml.IntoElem();
					Contact contact;
					contact.name = xml.GetAttrib(_T("name"));
					contact.number = xml.GetAttrib(_T("number"));
					contact.firstname = xml.GetAttrib(_T("firstname"));
					contact.lastname = xml.GetAttrib(_T("lastname"));
					contact.phone = xml.GetAttrib(_T("phone"));
					contact.mobile = xml.GetAttrib(_T("mobile"));
					contact.email = xml.GetAttrib(_T("email"));
					contact.address = xml.GetAttrib(_T("address"));
					contact.city = xml.GetAttrib(_T("city"));
					contact.state = xml.GetAttrib(_T("state"));
					contact.zip = xml.GetAttrib(_T("zip"));
					contact.comment = xml.GetAttrib(_T("comment"));
					contact.id = xml.GetAttrib(_T("id"));
					CString rab;
					rab = xml.GetAttrib(_T("presence"));
					contact.presence = rab == _T("1");
					rab = xml.GetAttrib(_T("starred"));
					contact.starred = rab == _T("1");
					rab = xml.GetAttrib(_T("directory"));
					contact.directory = rab == _T("1");
					if (contact.presence || contact.directory) {
						contact.info = xml.GetAttrib(_T("info"));
					}
					if (!contact.number.IsEmpty()) {
						if (!IsFiltered(contact)) {
							ContactAdd(contact, FALSE, TRUE);
						}
					}
					xml.OutOfElem();
				}
			}
		}
	}
	else {
		// old
		CString key;
		CString val;
		LPTSTR ptr = val.GetBuffer(256);
		int i = 0;
		while (TRUE) {
			key.Format(_T("%d"), i);
			if (GetPrivateProfileString(_T("Contacts"), key, NULL, ptr, 256, accountSettings.iniFile)) {
				Contact contact;
				ContactDecode(ptr, contact);
				ContactAdd(contact, FALSE, TRUE);
			}
			else {
				break;
			}
			i++;
		}
		WritePrivateProfileSection(_T("Contacts"), NULL, accountSettings.iniFile);
		ContactsSave();
	}
	m_list.SortColumn(m_list.GetSortColumn(), m_list.IsAscending());
}

void Contacts::ContactDecode(CString str, Contact& contact)
{
	CString rab;
	int begin;
	int end;
	begin = 0;
	end = str.Find(';', begin);
	if (end != -1) {
		contact.number = str.Mid(begin, end - begin);
		begin = end + 1;
		end = str.Find(';', begin);
		if (end != -1) {
			contact.name = str.Mid(begin, end - begin);
			begin = end + 1;
			end = str.Find(';', begin);
			if (end != -1) {
				rab = str.Mid(begin, end - begin);
				contact.presence = rab == _T("1");
				begin = end + 1;
				end = str.Find(';', begin);
				if (end != -1) {
					rab = str.Mid(begin, end - begin);
				}
				else {
					rab = str.Mid(begin);
				}
				contact.directory = rab == _T("1");
			}
			else {
				rab = str.Mid(begin);
				contact.presence = rab == _T("1");
			}
		}
		else {
			contact.name = str.Mid(begin);
		}
	}
	else {
		contact.number = str;
		contact.name = contact.number;
	}
}

Contact* Contacts::FindContact(CString number, bool subscribed)
{
	POSITION pos = contacts.GetHeadPosition();
	while (pos) {
		Contact* contact = contacts.GetNext(pos);
		if (subscribed) {
			if (contact->presence) {
				CString commands;
				CString numberFormated = FormatNumber(contact->number, &commands, true);
				if (number == numberFormated) {
					return contact;
				}
			}
		}
		else {
			if (number == contact->number) {
				return contact;
			}
		}
	};
	return NULL;
}

CString Contacts::GetNameAltByNumber(const CString& numberRemote, const CString& numberLocal, const CString& name)
{
    CString nameAlt;
    if (MSIP::IsPSTNNnmber(numberRemote) && MSIP::IsPSTNNnmber(numberLocal)) {
        int offset1 = MSIP::GetNormalizedNumberOffset(numberRemote);
        int offset2 = MSIP::GetNormalizedNumberOffset(numberLocal);
        int len1 = numberRemote.GetLength();
        int len2 = numberLocal.GetLength();
        const CString* str1, * str2;
        if (len1 - offset1 > len2 - offset2) {
            str1 = &numberRemote;
            str2 = &numberLocal;
        }
        else {
            str1 = &numberLocal;
            str2 = &numberRemote;
            int tmp = len1;
            len1 = len2;
            len2 = tmp;
            tmp = offset1;
            offset1 = offset2;
            offset2 = tmp;
        }
        if ((len1 - offset1) - (len2 - offset2) <= 3) {
            if (wcsncmp(str1->GetString() + len1 - len2 + offset2, str2->GetString() + offset2, len2 - offset2) == 0) {
                nameAlt = name;
            }
        }
    }
    return nameAlt;
}

CString Contacts::GetNameByNumber(const CString &number)
{
	CString name;
	CString nameAlt;
	POSITION pos = contacts.GetHeadPosition();
	while (pos) {
		Contact* contact = contacts.GetNext(pos);
        CString stub;
        CString numberContact = FormatNumber(contact->number, &stub);
        SIPURI sipuri;
        MSIP::ParseSIPURI(numberContact, sipuri);
        numberContact = !sipuri.user.IsEmpty() ? sipuri.user : sipuri.domain;
        CString phoneContact;
        if (!contact->phone.IsEmpty()) {
            phoneContact = SanitizeNumber(contact->phone);
        }
        CString mobileContact;
        if (!contact->mobile.IsEmpty()) {
            mobileContact = SanitizeNumber(contact->mobile);
        }
        if (number == numberContact || number == phoneContact || number == mobileContact) {
            name = contact->name;
            break;
        }
        nameAlt = GetNameAltByNumber(number, numberContact, contact->name);
        if (nameAlt.IsEmpty() && !phoneContact.IsEmpty()) {
            nameAlt = GetNameAltByNumber(number, phoneContact, contact->name);
        }
        if (nameAlt.IsEmpty() && !mobileContact.IsEmpty()) {
            nameAlt = GetNameAltByNumber(number, mobileContact, contact->name);
        }
	};
	return !name.IsEmpty() ? name : nameAlt;
}

void Contacts::PresenceUnsubsribeOne(Contact* pContact)
{
	mainDlg->UnsubscribeNumber(pContact->number);
	PresenceReset(pContact);
}

void Contacts::PresenceSubscribe()
{
	POSITION pos = contacts.GetHeadPosition();
	while (pos) {
		Contact* contact = contacts.GetNext(pos);
		if (contact->presence) {
			mainDlg->SubsribeNumber(contact->number);
		}
	}
}

void Contacts::PresenceReset(Contact* pContact)
{
	if (!::IsWindow(this->m_hWnd)) {
		return;
	}
	if (IsFiltered()) {
		FilterReset();
	}
	CListCtrl* list = (CListCtrl*)GetDlgItem(IDC_CONTACTS);
	int n = list->GetItemCount();
	for (int i = 0; i < n; i++) {
		Contact* contact = (Contact*)list->GetItemData(i);
		if (!pContact || pContact == contact) {
			if (contact->image != MSIP_CONTACT_ICON_DEFAULT) {
				contact->info.Empty();
				list->SetItemText(i, 2, _T(""));
			}
			contact->image = MSIP_CONTACT_ICON_DEFAULT;
			contact->ringing = false;
			list->SetItem(i, 0, LVIF_IMAGE, 0, contact->image + (contact->starred ? 7 : 0), 0, 0, 0);
		}
	}
}

void Contacts::PresenceReceived(CString* buddyNumber, int image, bool ringing, CString* info, bool fromUsersDirectory)
{
	bool blink = false;
	CListCtrl* list = (CListCtrl*)GetDlgItem(IDC_CONTACTS);
	POSITION pos = contacts.GetHeadPosition();
	while (pos) {
		Contact* contact = contacts.GetNext(pos);
		if (contact->presence || fromUsersDirectory) {
			CString numberFormated;
			if (fromUsersDirectory) {
				numberFormated = contact->number;
			}
			else {
				CString commands;
				numberFormated = FormatNumber(contact->number, &commands, true);
			}
			if (*buddyNumber == numberFormated) {
				if (ringing) {
					blink = true;
				}
				contact->image = image;
				contact->ringing = ringing;
				contact->info = *info;
				LVFINDINFO findInfo;
				int i;
				findInfo.flags = LVFI_PARAM;
				findInfo.lParam = (LPARAM)contact;
				if ((i = list->FindItem(&findInfo)) != -1) {
					list->SetItem(i, 0, LVIF_IMAGE, 0, contact->image + (contact->starred ? 7 : 0), 0, 0, 0);
					list->SetItemText(i, 2, Translate(contact->info));
				}
			}
		}
	};
	if (blink) {
		if (!blinkTimer) {
			blinkTimer = SetTimer(IDT_TIMER_CONTACTS_BLINK, 500, NULL);
			OnTimerContactsBlink();
		}
	}
}

void Contacts::OnTimerContactsBlink()
{
	if (!blinkTimer) {
		return;
	}
	CListCtrl* list = (CListCtrl*)GetDlgItem(IDC_CONTACTS);
	int n = list->GetItemCount();
	bool ringing = false;
	for (int i = 0; i < n; i++) {
		Contact* contact = (Contact*)list->GetItemData(i);
		if (contact->ringing) {
			list->SetItem(i, 0, LVIF_IMAGE, 0, blinkState ? contact->image + (contact->starred ? 7 : 0) : MSIP_CONTACT_ICON_BLANK, 0, 0, 0);
			ringing = true;
		}
	}
	if (!ringing) {
		blinkTimer = NULL;
		KillTimer(IDT_TIMER_CONTACTS_BLINK);
		blinkState = false;
	}
	else {
		blinkState = !blinkState;
	}
}

void Contacts::SetCanditates()
{
	if (IsFiltered()) {
		FilterReset();
	}
	GetDlgItem(IDC_FILER_VALUE)->EnableWindow(FALSE);
	CListCtrl* list = (CListCtrl*)GetDlgItem(IDC_CONTACTS);
	int count = list->GetItemCount();
	for (int i = 0; i < count; i++)
	{
		Contact* pContact = (Contact*)list->GetItemData(i);
		if (pContact->directory) {
			pContact->candidate = true;
		}
	}
}
int Contacts::DeleteCanditates()
{
	if (IsFiltered()) {
		FilterReset();
	}
	CListCtrl* list = (CListCtrl*)GetDlgItem(IDC_CONTACTS);
	int count = list->GetItemCount();
	int deleted = 0;
	for (int i = 0; i < count; i++)
	{
		Contact* pContact = (Contact*)list->GetItemData(i);
		if (pContact->candidate) {
			ContactDelete(i);
			count--;
			i--;
			deleted++;
		}
	}
	GetDlgItem(IDC_FILER_VALUE)->EnableWindow(TRUE);
	return deleted;
}

